use clap::{Parser, Subcommand};
use serde_json::json;
use std::collections::HashMap;

use crate::audit::AuditStore;
use crate::executor;
use crate::model::{ExecuteRequest, ExtensionInvokeRequest};
use crate::policy::Policy;

#[derive(Parser)]
#[command(name = "my_sandbox", about = "Lightweight restricted command sandbox", version)]
pub struct Cli {
    #[arg(long, global = true, value_name = "FILE")]
    pub policy: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Run {
        #[arg(long)]
        cmd: String,

        #[arg(long = "arg", value_name = "ARG")]
        args: Vec<String>,

        #[arg(long)]
        cwd: Option<String>,

        #[arg(long, default_value = "10")]
        timeout: u64,

        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
    },
    Invoke {
        #[arg(long)]
        name: String,

        #[arg(long = "arg", value_name = "ARG")]
        args: Vec<String>,

        #[arg(long)]
        cwd: Option<String>,

        #[arg(long)]
        timeout: Option<u64>,

        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
    },
    Bench {
        #[arg(long, default_value = "echo")]
        cmd: String,

        #[arg(long = "arg", value_name = "ARG")]
        args: Vec<String>,

        #[arg(long, default_value = "1000")]
        n: usize,

        #[arg(long)]
        cwd: Option<String>,

        #[arg(long, default_value = "5")]
        timeout: u64,
    },
    InitRootfs {
        #[arg(long)]
        rootfs: Option<String>,

        #[arg(long)]
        busybox: Option<String>,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:3000")]
        addr: String,
    },
}

/// CLI 入口：根据子命令分发执行逻辑。
pub async fn run_cli(cli: Cli, policy: Policy) {
    match cli.command {
        Commands::Run { cmd, args, cwd, timeout, env } => {
            let env_map = parse_env(env);
            let req = ExecuteRequest {
                command: cmd,
                args,
                cwd,
                timeout_secs: Some(timeout),
                env: env_map,
            };

            let audit = AuditStore::in_memory();
            let result = executor::execute(req, &policy, &audit, "cli").await;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Commands::Invoke { name, args, cwd, timeout, env } => {
            let Some(spec) = policy.extension(&name) else {
                println!("{}", json!({ "error": "extension not found", "name": name }));
                return;
            };

            let req = ExtensionInvokeRequest {
                args,
                cwd,
                timeout_secs: timeout,
                env: parse_env(env),
            };

            let audit = AuditStore::in_memory();
            let result = executor::execute_extension(&name, spec, req, &policy, &audit, "cli-ext").await;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Commands::Bench { cmd, args, n, cwd, timeout } => {
            run_bench(cmd, args, n, cwd, timeout, policy).await;
        }
        Commands::InitRootfs { rootfs, busybox } => {
            let rootfs = resolve_rootfs(rootfs.as_deref(), &policy);
            match init_rootfs(&rootfs, busybox.as_deref()) {
                Ok(()) => println!("rootfs initialized at {}", rootfs),
                Err(e) => eprintln!("init-rootfs failed: {}", e),
            }
        }
        Commands::Serve { addr } => {
            run_server(addr, policy).await;
        }
    }
}

/// 解析 KEY=VALUE 形式的环境变量参数。
fn parse_env(env: Vec<String>) -> HashMap<String, String> {
    env.iter()
        .filter_map(|s| {
            let mut parts = s.splitn(2, '=');
            let k = parts.next()?.to_string();
            let v = parts.next().unwrap_or("").to_string();
            Some((k, v))
        })
        .collect()
}

/// 启动 HTTP 服务并挂载路由。
async fn run_server(addr: String, policy: Policy) {
    use crate::api::{router, AppState};
    use std::sync::Arc;

    let audit = AuditStore::with_file("audit.jsonl");
    let state = AppState {
        policy: Arc::new(policy),
        audit,
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("HTTP server listening on http://{}", addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

/// 监听 Ctrl+C 以触发优雅退出。
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// 简单基准测试：重复执行并统计延迟分位数。
async fn run_bench(cmd: String, args: Vec<String>, n: usize, cwd: Option<String>, timeout: u64, policy: Policy) {
    let audit = AuditStore::in_memory();
    let mut durations = Vec::with_capacity(n);

    for _ in 0..n {
        let req = ExecuteRequest {
            command: cmd.clone(),
            args: args.clone(),
            cwd: cwd.clone(),
            timeout_secs: Some(timeout),
            env: HashMap::new(),
        };
        let result = executor::execute(req, &policy, &audit, "bench").await;
        durations.push(result.duration_ms);
    }

    durations.sort_unstable();
    let p50 = percentile(&durations, 50);
    let p95 = percentile(&durations, 95);
    let p99 = percentile(&durations, 99);
    let avg = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<u64>() / durations.len() as u64
    };

    let report = serde_json::json!({
        "count": durations.len(),
        "avg_ms": avg,
        "p50_ms": p50,
        "p95_ms": p95,
        "p99_ms": p99
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

/// 计算指定分位数（p 为 0-100）。
fn percentile(data: &[u64], p: usize) -> u64 {
    if data.is_empty() {
        return 0;
    }
    let rank = ((p as f64 / 100.0) * data.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(data.len() - 1);
    data[idx]
}

/// 解析 rootfs 路径：命令行优先，其次策略文件，最后使用默认值。
fn resolve_rootfs(cli_rootfs: Option<&str>, policy: &Policy) -> String {
    if let Some(p) = cli_rootfs {
        return p.to_string();
    }
    if let Some(iso) = &policy.isolation {
        if let Some(p) = &iso.rootfs {
            if !p.trim().is_empty() {
                return p.clone();
            }
        }
    }
    "/var/lib/my_sandbox/rootfs".to_string()
}

/// 初始化最小 rootfs：复制 busybox、依赖库并创建全量命令链接。
fn init_rootfs(rootfs: &str, busybox_path: Option<&str>) -> Result<(), String> {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    let rootfs = Path::new(rootfs);
    let busybox = match busybox_path {
        Some(p) => PathBuf::from(p),
        None => {
            let out = Command::new("which")
                .arg("busybox")
                .output()
                .map_err(|e| format!("failed to run `which busybox`: {}", e))?;
            if !out.status.success() {
                return Err("busybox not found in PATH, install it or pass --busybox".into());
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                return Err("busybox not found in PATH, install it or pass --busybox".into());
            }
            PathBuf::from(s)
        }
    };

    fs::create_dir_all(rootfs.join("bin")).map_err(|e| e.to_string())?;
    fs::create_dir_all(rootfs.join("proc")).map_err(|e| e.to_string())?;

    let dest_busybox = rootfs.join("bin").join("busybox");
    fs::copy(&busybox, &dest_busybox).map_err(|e| format!("copy busybox failed: {}", e))?;

    let out = Command::new("ldd")
        .arg(&busybox)
        .output()
        .map_err(|e| format!("failed to run `ldd {}`: {}", busybox.display(), e))?;
    if !out.status.success() {
        return Err("`ldd` failed, cannot resolve busybox dependencies".into());
    }
    let ldd = String::from_utf8_lossy(&out.stdout);
    let mut libs = BTreeSet::new();
    for line in ldd.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("linux-vdso") {
            continue;
        }
        if let Some(pos) = line.find("=>") {
            let path = line[pos + 2..].trim();
            if let Some(path) = path.split_whitespace().next() {
                if path.starts_with('/') {
                    libs.insert(PathBuf::from(path));
                }
            }
        } else if line.starts_with('/') {
            if let Some(path) = line.split_whitespace().next() {
                libs.insert(PathBuf::from(path));
            }
        }
    }

    for lib in libs {
        let dest = rootfs.join(lib.strip_prefix("/").unwrap_or(&lib));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(&lib, &dest)
            .map_err(|e| format!("copy {} failed: {}", lib.display(), e))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let bin = rootfs.join("bin");
        let out = Command::new(&dest_busybox)
            .arg("--list")
            .output()
            .map_err(|e| format!("failed to run `busybox --list`: {}", e))?;
        if !out.status.success() {
            return Err("`busybox --list` failed".into());
        }
        let list = String::from_utf8_lossy(&out.stdout);
        for name in list.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
            let link = bin.join(name);
            if !link.exists() {
                let _ = symlink("busybox", &link);
            }
        }
    }

    Ok(())
}
