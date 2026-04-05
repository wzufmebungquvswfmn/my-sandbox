use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 执行请求
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecuteRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// 扩展能力调用请求
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtensionInvokeRequest {
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// 执行状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteStatus {
    /// 被策略拒绝，未执行
    Rejected,
    /// 执行成功（exit code 0）
    Succeeded,
    /// 执行完成但失败（exit code != 0）
    Failed,
    /// 超时被终止
    TimedOut,
}

/// 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResult {
    pub request_id: String,
    pub status: ExecuteStatus,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub reject_reason: Option<String>,
    pub created_at: String,
}
