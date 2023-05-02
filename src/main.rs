pub use controller;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let controller = controller::run();
    tokio::join!(controller);
    Ok(())
}
