use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::audit::{AuditRecord, AuditStore};
use crate::model::{ExecuteRequest, ExecuteResult, ExecuteStatus, ExtensionInvokeRequest};
use crate::policy::{ExtensionSpec, Policy, PolicyViolation};


/// 执行普通命令请求并返回结果（含审计记录）。
pub async fn execute(
    req: ExecuteRequest,
    policy: &Policy,
    audit: &AuditStore,
    source: &str,
) -> ExecuteResult {
    execute_internal(req, policy, audit, source, None, false).await
}

/// 执行扩展能力请求（基于扩展定义拼装命令）。
pub async fn execute_extension(
    name: &str,
    spec: ExtensionSpec,
    invoke: ExtensionInvokeRequest,
    policy: &Policy,
    audit: &AuditStore,
    source: &str,
) -> ExecuteResult {
    let env_keys = policy.allowed_env_keys_for_extension(&spec);

    let mut args = spec.args.clone();
    args.extend(invoke.args);

    let cwd = invoke.cwd.or(spec.cwd);
    let timeout_secs = invoke.timeout_secs.or(spec.timeout_secs);

    let mut env = spec.env.clone();
    env.extend(invoke.env.into_iter());

    let req = ExecuteRequest {
        command: spec.command,
        args,
        cwd,
        timeout_secs,
        env,
    };
    let source = format!("{}:{}", source, name);
    execute_internal(req, policy, audit, &source, Some(env_keys), true).await
}

/// 核心执行逻辑：策略校验、进程启动、超时与输出收集。
async fn execute_internal(
    req: ExecuteRequest,
    policy: &Policy,
    audit: &AuditStore,
    source: &str,
    env_keys: Option<Vec<String>>,
    skip_command_check: bool,
) -> ExecuteResult {
    let request_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let timeout_secs = req.timeout_secs.unwrap_or(10).min(policy.max_timeout_secs);

    let command_name = req.command.clone();
    let args_copy = req.args.clone();
    let cwd_copy = req.cwd.clone();
    let source = source.to_string();

    let validation = if skip_command_check {
        policy.validate_extension(req.cwd.as_deref(), req.timeout_secs)
    } else {
        policy.validate(&req.command, req.cwd.as_deref(), req.timeout_secs)
    };

    let reject_result = match validation {
        Err(PolicyViolation::CommandEmpty) => {
            let reason = "command is empty".to_string();
            tracing::warn!(request_id, "rejected: {}", reason);
            Some(ExecuteResult {
                request_id: request_id.clone(),
                status: ExecuteStatus::Rejected,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms: 0,
                reject_reason: Some(reason),
                created_at: created_at.clone(),
            })
        }
        Err(PolicyViolation::CommandNotAllowed(reason)) => {
            tracing::warn!(request_id, command = %req.command, "rejected: {}", reason);
            Some(ExecuteResult {
                request_id: request_id.clone(),
                status: ExecuteStatus::Rejected,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms: 0,
                reject_reason: Some(reason),
                created_at: created_at.clone(),
            })
        }
        Err(PolicyViolation::TimeoutExceeded(max)) => {
            let reason = format!("requested timeout exceeds max allowed ({} secs)", max);
            tracing::warn!(request_id, "rejected: {}", reason);
            Some(ExecuteResult {
                request_id: request_id.clone(),
                status: ExecuteStatus::Rejected,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms: 0,
                reject_reason: Some(reason),
                created_at: created_at.clone(),
            })
        }
        Ok(_) => None,
    };

    if let Some(result) = reject_result {
        audit.record(AuditRecord {
            source,
            command: command_name,
            args: args_copy,
            cwd: cwd_copy,
            result: result.clone(),
        });
        return result;
    }

    let filtered_env = match env_keys {
        Some(keys) => policy.filter_env_with_keys(&req.env, &keys),
        None => policy.filter_env(&req.env),
    };

    #[cfg(target_os = "windows")]
    const WINDOWS_BUILTINS: &[&str] = &[
        "echo", "dir", "type", "cls", "copy", "move", "del", "mkdir", "rmdir",
        "set", "date", "time", "ver", "whoami", "pwd",
    ];

    #[cfg(target_os = "windows")]
    let mut cmd = {
        if WINDOWS_BUILTINS.contains(&req.command.to_lowercase().as_str()) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&req.command).args(&req.args);
            c
        } else {
            let mut c = Command::new(&req.command);
            c.args(&req.args);
            c
        }
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = Command::new(&req.command);
        c.args(&req.args);
        c
    };
    cmd.env_clear();
    for (k, v) in &filtered_env {
        cmd.env(k, v);
    }
    if let Some(ref cwd) = req.cwd {
        cmd.current_dir(cwd);
    }

    // Linux/WSL2: set up namespace + chroot isolation before exec.
    #[cfg(unix)]
    if let Some(iso) = &policy.isolation {
        if iso.enabled {
            let iso = iso.clone();
            unsafe {
                cmd.pre_exec(move || {
                    use nix::mount::{mount, MsFlags};
                    use nix::sched::{unshare, CloneFlags};
                    use nix::unistd::{chdir, chroot};

                // Create new namespaces for process isolation.
                let mut flags = CloneFlags::CLONE_NEWNS
                    | CloneFlags::CLONE_NEWPID
                    | CloneFlags::CLONE_NEWUTS
                    | CloneFlags::CLONE_NEWIPC;
                if iso.net_namespace {
                    flags |= CloneFlags::CLONE_NEWNET;
                }

                // Detach from host namespaces.
                unshare(flags).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                // Make mounts private so changes don't propagate to the host.
                mount::<str, str, str, str>(
                    None,
                    "/",
                    None,
                    MsFlags::MS_REC | MsFlags::MS_PRIVATE,
                    None,
                )
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                // Switch into the provided root filesystem.
                if let Some(rootfs) = &iso.rootfs {
                    chroot(rootfs.as_str())
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    chdir("/").map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                }

                if iso.mount_proc {
                    // Provide /proc inside the sandbox.
                    let _ = std::fs::create_dir_all("/proc");
                    mount(
                        Some("proc"),
                        "/proc",
                        Some("proc"),
                        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                        None::<&str>,
                    )
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                }

                // Disallow acquiring new privileges.
                let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
                if rc != 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "prctl(PR_SET_NO_NEW_PRIVS) failed",
                    ));
                }

                if iso.seccomp_strict {
                    // Optional strict syscall filter (very restrictive).
                    let rc = unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_STRICT) };
                    if rc != 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "prctl(PR_SET_SECCOMP, STRICT) failed",
                        ));
                    }
                }

                    Ok(())
                });
            }
        }
    }

    // Cap total output to avoid unbounded memory usage.
    const MAX_OUTPUT_BYTES: usize = 256 * 1024;

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let start = Instant::now();
    tracing::info!(request_id, command = %req.command, args = ?req.args, "executing");

    let mut child = match cmd.spawn() {
        Err(e) => {
            tracing::error!(request_id, error = %e, "failed to spawn process");
            let r = ExecuteResult {
                request_id,
                status: ExecuteStatus::Failed,
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: None,
                duration_ms: 0,
                reject_reason: None,
                created_at,
            };
            audit.record(AuditRecord {
                source,
                command: command_name,
                args: args_copy,
                cwd: cwd_copy,
                result: r.clone(),
            });
            return r;
        }
        Ok(c) => c,
    };

    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();

    // Collect stdout/stderr concurrently with a hard timeout.
    let wait_result = timeout(Duration::from_secs(timeout_secs), async {
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let mut out_tmp = vec![0u8; 4096];
        let mut err_tmp = vec![0u8; 4096];

        loop {
            tokio::select! {
                n = stdout_pipe.read(&mut out_tmp) => {
                    match n {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let take = n.min(MAX_OUTPUT_BYTES.saturating_sub(out_buf.len()));
                            out_buf.extend_from_slice(&out_tmp[..take]);
                        }
                    }
                }
                n = stderr_pipe.read(&mut err_tmp) => {
                    match n {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let take = n.min(MAX_OUTPUT_BYTES.saturating_sub(err_buf.len()));
                            err_buf.extend_from_slice(&err_tmp[..take]);
                        }
                    }
                }
            }
            if out_buf.len() >= MAX_OUTPUT_BYTES && err_buf.len() >= MAX_OUTPUT_BYTES {
                break;
            }
        }

        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, out_buf, err_buf))
    })
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match wait_result {
        Err(_elapsed) => {
            let _ = child.kill().await;
            tracing::warn!(request_id, duration_ms, "timed out, process killed");
            let r = ExecuteResult {
                request_id,
                status: ExecuteStatus::TimedOut,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                duration_ms,
                reject_reason: Some(format!("execution timed out after {} secs", timeout_secs)),
                created_at,
            };
            audit.record(AuditRecord {
                source,
                command: command_name,
                args: args_copy,
                cwd: cwd_copy,
                result: r.clone(),
            });
            r
        }
        Ok(Err(e)) => {
            tracing::error!(request_id, error = %e, "process wait error");
            let r = ExecuteResult {
                request_id,
                status: ExecuteStatus::Failed,
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: None,
                duration_ms,
                reject_reason: None,
                created_at,
            };
            audit.record(AuditRecord {
                source,
                command: command_name,
                args: args_copy,
                cwd: cwd_copy,
                result: r.clone(),
            });
            r
        }
        Ok(Ok((exit_status, out_buf, err_buf))) => {
            let stdout = String::from_utf8_lossy(&out_buf).to_string();
            let stderr = String::from_utf8_lossy(&err_buf).to_string();
            let exit_code = exit_status.code();
            let status = if exit_status.success() {
                ExecuteStatus::Succeeded
            } else {
                ExecuteStatus::Failed
            };
            tracing::info!(request_id, ?status, exit_code, duration_ms, "completed");
            let r = ExecuteResult {
                request_id,
                status,
                stdout,
                stderr,
                exit_code,
                duration_ms,
                reject_reason: None,
                created_at,
            };
            audit.record(AuditRecord {
                source,
                command: command_name,
                args: args_copy,
                cwd: cwd_copy,
                result: r.clone(),
            });
            r
        }
    }
}
