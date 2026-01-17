#[tokio::main]
async fn main() -> anyhow::Result<()> {
    speakv::server::run_server().await
}
