use anyhow::Result;
use std::{path::PathBuf, time::Duration};
use telegram_codex_bridge::rpc::Connection;

#[tokio::main]
async fn main() -> Result<()> {
    let executable = std::env::var_os("CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| "codex".into());
    let connection = Connection::spawn(
        &executable,
        &std::env::current_dir()?,
        Duration::from_secs(30),
    )
    .await?;
    println!("Codex App Server handshake successful");
    connection.shutdown().await;
    Ok(())
}
