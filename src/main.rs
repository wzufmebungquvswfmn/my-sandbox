mod api;
mod audit;
mod cli;
mod executor;
mod model;
mod policy;
mod tests;

use clap::Parser;
use cli::{Cli, run_cli};
use policy::Policy;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "my_sandbox=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // 优先加载 --policy 指定的文件，否则尝试 sandbox.toml，再回退默认值
    let policy_path = cli.policy.as_deref().unwrap_or("sandbox.toml");
    let policy = Policy::load(policy_path);

    run_cli(cli, policy).await;
}
