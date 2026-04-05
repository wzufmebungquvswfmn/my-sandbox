#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use crate::audit::AuditStore;
    use crate::executor::{execute, execute_extension};
    use crate::model::{ExecuteRequest, ExecuteStatus, ExtensionInvokeRequest};
    use crate::policy::{ExtensionSpec, Policy};
    use std::fs;
    use std::thread::sleep;
    use std::time::Duration;

    fn default_req(cmd: &str, args: Vec<&str>) -> ExecuteRequest {
        ExecuteRequest {
            command: cmd.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            timeout_secs: Some(5),
            env: HashMap::new(),
        }
    }

    fn audit() -> AuditStore {
        AuditStore::in_memory()
    }

    #[tokio::test]
    async fn test_echo_succeeds() {
        let policy = Policy::default();
        let req = default_req("echo", vec!["hello"]);
        let result = execute(req, &policy, &audit(), "test").await;
        assert_eq!(result.status, ExecuteStatus::Succeeded);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_command_not_in_whitelist_rejected() {
        let policy = Policy::default();
        let req = default_req("rm", vec!["-rf", "/"]);
        let result = execute(req, &policy, &audit(), "test").await;
        assert_eq!(result.status, ExecuteStatus::Rejected);
        assert!(result.reject_reason.is_some());
    }

    #[tokio::test]
    async fn test_empty_command_rejected() {
        let policy = Policy::default();
        let req = default_req("", vec![]);
        let result = execute(req, &policy, &audit(), "test").await;
        assert_eq!(result.status, ExecuteStatus::Rejected);
        assert!(result.reject_reason.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn test_timeout_exceeded_policy_rejected() {
        let policy = Policy::default();
        let mut req = default_req("echo", vec!["hi"]);
        req.timeout_secs = Some(999);
        let result = execute(req, &policy, &audit(), "test").await;
        assert_eq!(result.status, ExecuteStatus::Rejected);
    }

    #[tokio::test]
    async fn test_env_filter() {
        let policy = Policy::default();
        let mut env = HashMap::new();
        env.insert("SECRET_KEY".to_string(), "should_not_pass".to_string());
        env.insert("MODE".to_string(), "test".to_string());
        let filtered = policy.filter_env(&env);
        assert!(!filtered.contains_key("SECRET_KEY"));
        assert_eq!(filtered.get("MODE").unwrap(), "test");
    }

    #[tokio::test]
    async fn test_audit_records_execution() {
        let policy = Policy::default();
        let store = AuditStore::in_memory();
        let req = default_req("echo", vec!["audit-test"]);
        let result = execute(req, &policy, &store, "test").await;
        let records = store.list();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].result.request_id, result.request_id);
        assert_eq!(records[0].source, "test");
    }

    #[tokio::test]
    async fn test_audit_find_by_id() {
        let policy = Policy::default();
        let store = AuditStore::in_memory();
        let req = default_req("echo", vec!["find-me"]);
        let result = execute(req, &policy, &store, "test").await;
        let found = store.find(&result.request_id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().result.request_id, result.request_id);
    }

    #[tokio::test]
    async fn test_extension_invoke_succeeds() {
        let policy = Policy::default();
        let store = AuditStore::in_memory();
        let spec = ExtensionSpec {
            command: "python".to_string(),
            args: vec!["-c".to_string(), "print('ext-ok')".to_string()],
            cwd: None,
            timeout_secs: Some(5),
            env: HashMap::new(),
            allowed_env_keys: None,
        };
        let invoke = ExtensionInvokeRequest {
            args: vec![],
            cwd: None,
            timeout_secs: None,
            env: HashMap::new(),
        };
        let result = execute_extension("ext", spec, invoke, &policy, &store, "test").await;
        assert_eq!(result.status, ExecuteStatus::Succeeded);
        assert!(result.stdout.contains("ext-ok"));
    }

    #[tokio::test]
    async fn test_batch_execute_concurrent() {
        let policy = Policy::default();
        let store = AuditStore::in_memory();
        let mut handles = Vec::new();
        for i in 0..5 {
            let policy = policy.clone();
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                let req = ExecuteRequest {
                    command: "echo".to_string(),
                    args: vec![format!("batch-{}", i)],
                    cwd: None,
                    timeout_secs: Some(5),
                    env: HashMap::new(),
                };
                execute(req, &policy, &store, "test").await
            }));
        }
        for h in handles {
            let r = h.await.unwrap();
            assert_eq!(r.status, ExecuteStatus::Succeeded);
        }
    }

    #[tokio::test]
    async fn test_output_truncation_max_bytes() {
        let policy = Policy::default();
        let store = AuditStore::in_memory();
        let script = "print('a'*300000)";
        let req = ExecuteRequest {
            command: "python".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: None,
            timeout_secs: Some(5),
            env: HashMap::new(),
        };
        let result = execute(req, &policy, &store, "test").await;
        assert_eq!(result.status, ExecuteStatus::Succeeded);
        assert_eq!(result.stdout.len(), 256 * 1024);
    }

    #[tokio::test]
    async fn test_timeout_kills_process() {
        let policy = Policy::default();
        let store = AuditStore::in_memory();
        let mut dir = std::env::temp_dir();
        dir.push("my_sandbox_test_timeout");
        let _ = fs::create_dir_all(&dir);
        let start_file = dir.join("start.txt");
        let end_file = dir.join("end.txt");
        let _ = fs::remove_file(&start_file);
        let _ = fs::remove_file(&end_file);

        let script = format!(
            "import time; open(r'{}','w').write('start'); time.sleep(5); open(r'{}','w').write('end')",
            start_file.display(),
            end_file.display()
        );
        let req = ExecuteRequest {
            command: "python".to_string(),
            args: vec!["-c".to_string(), script],
            cwd: None,
            timeout_secs: Some(1),
            env: HashMap::new(),
        };
        let result = execute(req, &policy, &store, "test").await;
        assert_eq!(result.status, ExecuteStatus::TimedOut);
        sleep(Duration::from_secs(2));
        assert!(start_file.exists());
        assert!(!end_file.exists());

        let _ = fs::remove_file(&start_file);
        let _ = fs::remove_file(&end_file);
        let _ = fs::remove_dir_all(&dir);
    }
}
