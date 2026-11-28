#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if let Some(result) = remoteops_agent::handle_ssh_trust_cli(&args).await {
        println!("{}", serde_json::to_string(&result?)?);
        return Ok(());
    }
    remoteops_agent::run_cli().await
}
