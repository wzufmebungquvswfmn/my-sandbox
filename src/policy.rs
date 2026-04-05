use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub allowed_env_keys: Option<Vec<String>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct IsolationSpec {
    #[serde(default)]
    pub enabled: bool,
    pub rootfs: Option<String>,
    #[serde(default = "default_mount_proc")]
    pub mount_proc: bool,
    #[serde(default)]
    pub net_namespace: bool,
    #[serde(default)]
    pub seccomp_strict: bool,
}

fn default_mount_proc() -> bool { true }

/// 执行策略配置（可从 TOML 文件加载）
#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    /// 允许执行的命令列表
    pub allowed_commands: Vec<String>,
    /// 最大超时秒数
    #[serde(default = "default_max_timeout")]
    pub max_timeout_secs: u64,
    /// 允许传入的环境变量 key 列表
    #[serde(default = "default_env_keys")]
    pub allowed_env_keys: Vec<String>,
    #[serde(default)]
    pub extensions: HashMap<String, ExtensionSpec>,
    #[allow(dead_code)]
    #[serde(default)]
    pub isolation: Option<IsolationSpec>,
}

fn default_max_timeout() -> u64 { 30 }

fn default_env_keys() -> Vec<String> {
    vec!["PATH".into(), "HOME".into(), "USER".into(), "LANG".into(), "MODE".into()]
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allowed_commands: vec![
                "echo".into(), "python".into(), "python3".into(),
                "node".into(), "cargo".into(), "ls".into(),
                "cat".into(), "pwd".into(), "date".into(), "whoami".into(),
            ],
            max_timeout_secs: 30,
            allowed_env_keys: default_env_keys(),
            extensions: HashMap::new(),
            isolation: None,
        }
    }
}

impl Policy {
    /// 从 TOML 文件加载策略，文件不存在时回退到默认值
    pub fn load(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(p) => {
                    tracing::info!("policy loaded from {}", path);
                    p
                }
                Err(e) => {
                    tracing::warn!("failed to parse policy file {}: {}, using defaults", path, e);
                    Self::default()
                }
            },
            Err(_) => {
                tracing::info!("policy file {} not found, using defaults", path);
                Self::default()
            }
        }
    }
}

pub enum PolicyViolation {
    CommandEmpty,
    CommandNotAllowed(String),
    TimeoutExceeded(u64),
}

impl Policy {
    /// 校验请求是否符合策略
    pub fn validate(
        &self,
        command: &str,
        cwd: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> Result<(), PolicyViolation> {
        self.validate_command(command)?;
        let _ = cwd;
        self.validate_common(timeout_secs)
    }

    pub fn validate_extension(
        &self,
        cwd: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> Result<(), PolicyViolation> {
        let _ = cwd;
        self.validate_common(timeout_secs)
    }

    fn validate_command(&self, command: &str) -> Result<(), PolicyViolation> {
        if command.trim().is_empty() {
            return Err(PolicyViolation::CommandEmpty);
        }
        if !self.allowed_commands.iter().any(|c| c == command) {
            return Err(PolicyViolation::CommandNotAllowed(format!(
                "command '{}' is not in the allowed list",
                command
            )));
        }
        Ok(())
    }

    fn validate_common(&self, timeout_secs: Option<u64>) -> Result<(), PolicyViolation> {
        if let Some(t) = timeout_secs {
            if t > self.max_timeout_secs {
                return Err(PolicyViolation::TimeoutExceeded(self.max_timeout_secs));
            }
        }

        Ok(())
    }

    pub fn extension(&self, name: &str) -> Option<ExtensionSpec> {
        self.extensions.get(name).cloned()
    }

    pub fn allowed_env_keys_for_extension(&self, spec: &ExtensionSpec) -> Vec<String> {
        spec.allowed_env_keys
            .clone()
            .unwrap_or_else(|| self.allowed_env_keys.clone())
    }

    /// 过滤环境变量，只保留允许的 key
    pub fn filter_env(
        &self,
        env: &std::collections::HashMap<String, String>,
    ) -> std::collections::HashMap<String, String> {
        self.filter_env_with_keys(env, &self.allowed_env_keys)
    }

    pub fn filter_env_with_keys(
        &self,
        env: &std::collections::HashMap<String, String>,
        allowed_keys: &[String],
    ) -> std::collections::HashMap<String, String> {
        if allowed_keys.is_empty() {
            return std::collections::HashMap::new();
        }
        env.iter()
            .filter(|(k, _)| allowed_keys.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
