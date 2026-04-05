use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::AuditStore;
use crate::executor;
use crate::model::{ExecuteRequest, ExecuteStatus, ExtensionInvokeRequest};
use crate::policy::Policy;

#[derive(Clone)]
pub struct AppState {
    pub policy: Arc<Policy>,
    pub audit: AuditStore,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/execute", post(execute))
        .route("/execute/batch", post(execute_batch))
        .route("/extensions", get(list_extensions))
        .route("/extensions/{name}/invoke", post(invoke_extension))
        .route("/executions", get(list_executions))
        .route("/executions/{id}", get(get_execution))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn execute(
    State(state): State<AppState>,
    Json(req): Json<ExecuteRequest>,
) -> (StatusCode, Json<Value>) {
    let result = executor::execute(req, &state.policy, &state.audit, "api").await;
    (StatusCode::OK, Json(serde_json::to_value(&result).unwrap()))
}

async fn execute_batch(
    State(state): State<AppState>,
    Json(reqs): Json<Vec<ExecuteRequest>>,
) -> (StatusCode, Json<Value>) {
    if reqs.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "empty batch" })));
    }
    if reqs.len() > 20 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "batch size exceeds limit of 20" })),
        );
    }

    let mut handles = Vec::new();
    for req in reqs {
        let policy = Arc::clone(&state.policy);
        let audit = state.audit.clone();
        handles.push(tokio::spawn(async move {
            executor::execute(req, &policy, &audit, "api-batch").await
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        if let Ok(r) = h.await {
            results.push(r);
        }
    }

    (StatusCode::OK, Json(json!({ "results": results })))
}

async fn list_extensions(State(state): State<AppState>) -> Json<Value> {
    let mut names: Vec<String> = state.policy.extensions.keys().cloned().collect();
    names.sort();
    Json(json!({ "total": names.len(), "extensions": names }))
}

async fn invoke_extension(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<ExtensionInvokeRequest>,
) -> (StatusCode, Json<Value>) {
    let spec = match state.policy.extension(&name) {
        Some(spec) => spec,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "extension not found", "name": name })),
            )
        }
    };

    let result =
        executor::execute_extension(&name, spec, req, &state.policy, &state.audit, "api-ext")
            .await;
    (StatusCode::OK, Json(serde_json::to_value(&result).unwrap()))
}

async fn list_executions(State(state): State<AppState>) -> Json<Value> {
    let mut records = state.audit.list();
    records.reverse();
    records.truncate(100);
    Json(json!({ "total": records.len(), "records": records }))
}

async fn get_execution(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    match state.audit.find(&id) {
        Some(rec) => (StatusCode::OK, Json(serde_json::to_value(&rec).unwrap())),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "execution not found", "request_id": id })),
        ),
    }
}

async fn metrics(State(state): State<AppState>) -> Json<Value> {
    let records = state.audit.list();
    let total = records.len();
    let succeeded = records.iter().filter(|r| r.result.status == ExecuteStatus::Succeeded).count();
    let failed = records.iter().filter(|r| r.result.status == ExecuteStatus::Failed).count();
    let rejected = records.iter().filter(|r| r.result.status == ExecuteStatus::Rejected).count();
    let timed_out = records.iter().filter(|r| r.result.status == ExecuteStatus::TimedOut).count();

    let durations: Vec<u64> = records
        .iter()
        .filter(|r| r.result.status != ExecuteStatus::Rejected)
        .map(|r| r.result.duration_ms)
        .collect();

    let avg_ms = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<u64>() / durations.len() as u64
    };

    let p95_ms = percentile(&durations, 95);
    let p99_ms = percentile(&durations, 99);

    Json(json!({
        "total": total,
        "succeeded": succeeded,
        "failed": failed,
        "rejected": rejected,
        "timed_out": timed_out,
        "duration_ms": {
            "avg": avg_ms,
            "p95": p95_ms,
            "p99": p99_ms,
        }
    }))
}

fn percentile(data: &[u64], p: usize) -> u64 {
    if data.is_empty() {
        return 0;
    }
    let mut data = data.to_vec();
    data.sort_unstable();
    let rank = ((p as f64 / 100.0) * data.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(data.len() - 1);
    data[idx]
}
