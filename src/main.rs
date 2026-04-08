#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wf_engine::run_cli().await
}
