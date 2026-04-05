use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};

use crate::model::ExecuteResult;

/// 单条审计记录（在 ExecuteResult 基础上补充调用来源）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub source: String, // "cli" 或 "api"
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub result: ExecuteResult,
}

/// 内存 + 文件双写的审计存储
#[derive(Clone)]
pub struct AuditStore {
    inner: Arc<Mutex<Vec<AuditRecord>>>,
    log_path: Option<String>,
}

impl AuditStore {
    /// 不持久化，仅内存（测试用）
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            log_path: None,
        }
    }

    /// 同时写入内存和文件（JSON Lines 格式）
    pub fn with_file(path: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            log_path: Some(path.into()),
        }
    }

    /// 写入一条审计记录
    pub fn record(&self, rec: AuditRecord) {
        // 持久化到文件
        if let Some(ref path) = self.log_path {
            if let Ok(line) = serde_json::to_string(&rec) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{}", line);
                    let _ = f.flush();
                    let _ = f.sync_data();
                }
            }
        }

        // 写入内存（保留最近 1000 条）
        let mut store = self.inner.lock().unwrap();
        store.push(rec);
        if store.len() > 1000 {
            store.remove(0);
        }
    }

    /// 返回所有内存中的记录（最新的在后）
    pub fn list(&self) -> Vec<AuditRecord> {
        self.inner.lock().unwrap().clone()
    }

    /// 按 request_id 查找
    pub fn find(&self, request_id: &str) -> Option<AuditRecord> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.result.request_id == request_id)
            .cloned()
    }
}
