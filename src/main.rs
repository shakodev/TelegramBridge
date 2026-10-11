use anyhow::{Context, Result};
use std::{path::PathBuf, time::Duration};
use telegram_codex_bridge::{
    app::App,
    config::Config,
    rpc::{Connection, Event},
    store::Store,
    telegram,
};
use teloxide::Bot;
use tokio::sync::mpsc;

async fn reconnect(executable: PathBuf, cwd: PathBuf, timeout: Duration) -> Connection {
    let mut delay = 1;
    loop {
        match Connection::spawn(&executable, &cwd, timeout).await {
            Ok(connection) => return connection,
            Err(_) => {
                tracing::warn!(
                    "Codex unavailable; retrying in {delay}s (check CODEX_BIN and Codex configuration)"
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(30);
            }
        }
    }
}
async fn event(connection: &mut Option<Connection>) -> Option<Event> {
    match connection {
        Some(c) => c.events.recv().await,
        None => std::future::pending().await,
    }
}
#[tokio::main(worker_threads = 2)]
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
        let c = Connection::spawn(
            &executable,
            &std::env::current_dir()?,
            Duration::from_secs(30),
        )
        .await?;
        println!("Codex App Server handshake successful");
        c.shutdown().await;
        return Ok(());
    }
    let config = Config::from_env()?;
    let store = Store::open(&config.database)?;
    let bot = Bot::new(&config.token);
    let executable = config.codex.clone();
    let cwd = config.projects.values().next().unwrap().clone();
    let timeout = config.rpc_timeout;
    let initial = tokio::select! {
        _=tokio::signal::ctrl_c()=>return Ok(()),
        c=reconnect(executable.clone(),cwd.clone(),timeout)=>c,
    };
    let mut app = App {
        sessions: store.load_sessions()?,
        rpc: initial.rpc.clone(),
        outgoing: store.clone(),
        approvals: Default::default(),
        progress: Default::default(),
        config,
    };
    // On startup, tell owners which sessions need inspection after interruption.
    for s in app
        .sessions
        .entries
        .values()
        .filter(|s| s.status.contains("unknown"))
    {
        app.say(s.chat,format!("#{}: recovered after interruption; inspect /status and /resume {}. No prompt was replayed.",s.id,s.id))?;
    }
    store.save_sessions(&app.sessions)?;
    let mut connection = Some(initial);
    let mut reconnecting: Option<tokio::task::JoinHandle<Connection>> = None;
    let (updates_tx, mut updates) = mpsc::channel(64);
    let (receipt_tx, mut receipts) = mpsc::channel(128);
    let mut poll = tokio::spawn(telegram::poll(bot.clone(), store.clone(), updates_tx));
    let mut delivery = tokio::spawn(telegram::deliver(
        bot,
        store.clone(),
        receipt_tx,
        app.config.users.clone(),
    ));
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                result = &mut poll => {
                    result.context("Telegram polling task crashed")??;
                    anyhow::bail!("Telegram polling stopped");
                }
                result = &mut delivery => {
                    result.context("Telegram delivery task crashed")??;
                    anyhow::bail!("Telegram delivery stopped");
                }
                Some(receipt) = receipts.recv() => recover_transport(app.receipt(receipt).await)?,
                _ = tick.tick() => {
                    if !app.rpc.is_closed() { recover_transport(app.tick().await)?; }
                }
                Some(incoming) = updates.recv() => {
                    if store.claim_update(incoming.update.id.0)? {
                        recover_transport(app.update(incoming.update).await)?;
                        store.save_sessions(&app.sessions)?;
                    }
                    let _ = incoming.ack.send(());
                }
                next = event(&mut connection) => match next {
                    Some(Event::Message(message)) => recover_transport(app.event(message).await)?,
                    _ => {
                        if let Some(old) = connection.take() { old.shutdown().await; }
                        app.disconnected()?;
                        reconnecting = Some(tokio::spawn(reconnect(executable.clone(),cwd.clone(),timeout)));
                    }
                },
                result = async {
                    match &mut reconnecting {
                        Some(task) => task.await,
                        None => std::future::pending().await,
                    }
                } => {
                    let c = result.context("Codex reconnect task failed")?;
                    app.rpc = c.rpc.clone();
                    connection = Some(c);
                    reconnecting = None;
                    tracing::info!("Codex connected; sessions will resume on demand");
                }
            }
        }
        Ok(())
    }.await;
    app.rpc.stop();
    if let Some(task) = reconnecting {
        task.abort();
    }
    poll.abort();
    delivery.abort();
    if let Some(c) = connection {
        c.shutdown().await;
    }
    app.disconnected()?;
    result
}

fn recover_transport(result: Result<()>) -> Result<()> {
    match result {
        Err(error) if error.is::<telegram_codex_bridge::rpc::Disconnected>() => {
            tracing::warn!("Codex transport lost; pending actions will not replay");
            Ok(())
        }
        other => other,
    }
}
