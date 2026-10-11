use anyhow::Result;
use std::{path::PathBuf, time::Duration};
use telegram_codex_bridge::{
    app::App,
    config::Config,
    rpc::{Connection, Event},
    sessions::Sessions,
    telegram,
};
use teloxide::Bot;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "telegram_codex_bridge=info".into()),
        )
        .init();
    if std::env::args().any(|a| a == "--check-codex") {
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
        return Ok(());
    }
    let config = Config::from_env()?;
    let bot = Bot::new(&config.token);
    let cwd = config.projects.values().next().unwrap();
    let mut connection = Connection::spawn(&config.codex, cwd, config.rpc_timeout).await?;
    let (updates_tx, mut updates) = mpsc::channel(64);
    let (outgoing, output_rx) = mpsc::channel(128);
    let poll = tokio::spawn(telegram::poll(bot.clone(), updates_tx));
    let delivery = tokio::spawn(telegram::deliver(bot, output_rx));
    let mut app = App {
        config,
        sessions: Sessions::default(),
        rpc: connection.rpc.clone(),
        outgoing,
    };
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(update) = updates.recv() => app.update(update).await?,
            event = connection.events.recv() => match event {
                Some(Event::Message(message)) => app.event(message).await?,
                _ => break,
            }
        }
    }
    poll.abort();
    delivery.abort();
    connection.shutdown().await;
    Ok(())
}
