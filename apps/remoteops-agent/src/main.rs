#[tokio::main]
async fn main() -> anyhow::Result<()> {
    remoteops_agent::run_cli().await
}

