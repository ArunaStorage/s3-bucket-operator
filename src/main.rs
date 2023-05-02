pub use controller;
use simple_logger::SimpleLogger;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    SimpleLogger::new().init().unwrap();
    let controller = controller::run().await;
    Ok(())
}
