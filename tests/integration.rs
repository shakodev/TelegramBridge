#![cfg(feature = "test-support")]
use serde_json::json;
use std::{path::Path, time::Duration};
use telegram_codex_bridge::{
    app::App,
    config::Config,
    rpc::{Connection, Event},
    store::Store,
    telegram::Receipt,
};
use teloxide::types::Update;

fn config(root: &Path) -> Config {
    Config {
        token: "test".into(),
        users: [7, 8].into(),
        projects: [("demo".into(), root.canonicalize().unwrap())].into(),
        codex: env!("CARGO_BIN_EXE_mock-codex").into(),
        database: root.join("bridge.sqlite"),
        sandbox: "read-only".into(),
        rpc_timeout: Duration::from_secs(3),
        approval_ttl: Duration::from_secs(60),
    }
}
async fn setup(root: &Path) -> (App, Connection) {
    let config = config(root);
    let store = Store::open(&config.database).unwrap();
    let c = Connection::spawn(&config.codex, root, config.rpc_timeout)
        .await
        .unwrap();
    (
        App {
            sessions: store.load_sessions().unwrap(),
            config,
            rpc: c.rpc.clone(),
            outgoing: store,
            approvals: Default::default(),
            progress: Default::default(),
        },
        c,
    )
}
fn message(user: u64, text: &str) -> Update {
    serde_json::from_str(&json!({"update_id":100,"message":{"message_id":1,"date":1,"from":{"id":user,"is_bot":false,"first_name":"Test"},"chat":{"id":user,"type":"private"},"text":text}}).to_string()).unwrap()
}
fn callback(user: u64, chat: i64, message: i32, data: &str) -> Update {
    serde_json::from_str(&json!({"update_id":200,"callback_query":{"id":"click","from":{"id":user,"is_bot":false,"first_name":"Test"},"chat_instance":"test","data":data,"message":{"message_id":message,"date":1,"chat":{"id":chat,"type":"private"},"text":"Review"}}}).to_string()).unwrap()
}
async fn pump(app: &mut App, c: &mut Connection, done: impl Fn(&App) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done(app) {
            match c.events.recv().await {
                Some(Event::Message(m)) => app.event(m).await.unwrap(),
                other => panic!("Unexpected disconnect: {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|e| {
        eprintln!("sessions: {:?}", app.sessions.entries);
        while let Some(job) = app.outgoing.next_job().unwrap() {
            eprintln!("out: {}", job.message.text);
            app.outgoing.delete(job.id).unwrap();
        }
        panic!("pump timeout: {e}");
    });
}
#[tokio::test]
async fn concurrent_sessions_interactive_approval_and_cross_user_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, mut c) = setup(dir.path()).await;
    app.update(message(999, "/new")).await.unwrap();
    assert!(app.sessions.entries.is_empty());
    app.update(message(7, "/new demo")).await.unwrap();
    app.update(message(7, "approval:command")).await.unwrap();
    app.update(message(7, "/new demo")).await.unwrap();
    app.update(message(7, "approval:permissions"))
        .await
        .unwrap();
    pump(&mut app, &mut c, |a| a.approvals.pending.len() == 2).await;
    let a = app
        .approvals
        .pending
        .values()
        .find(|a| a.session == 1)
        .unwrap()
        .clone();
    let b = app
        .approvals
        .pending
        .values()
        .find(|a| a.session == 2)
        .unwrap()
        .clone();
    app.receipt(Receipt {
        token: a.token.clone(),
        message: Some(10),
    })
    .await
    .unwrap();
    app.receipt(Receipt {
        token: b.token.clone(),
        message: Some(20),
    })
    .await
    .unwrap();
    app.update(callback(8, 8, 10, &format!("a:{}", a.token)))
        .await
        .unwrap();
    assert_eq!(app.approvals.pending.len(), 2);
    app.update(callback(7, 7, 20, &format!("a:{}", a.token)))
        .await
        .unwrap();
    assert_eq!(app.approvals.pending.len(), 2);
    app.update(callback(7, 7, 10, &format!("a:{}", a.token)))
        .await
        .unwrap();
    pump(&mut app, &mut c, |a| a.sessions.entries[&1].turn.is_none()).await;
    assert!(app.sessions.entries[&2].turn.is_some());
    assert_eq!(app.sessions.current(7, 7).unwrap().id, 2);
    app.update(callback(7, 7, 10, &format!("a:{}", a.token)))
        .await
        .unwrap();
    assert_eq!(app.approvals.pending.len(), 1);
    app.update(callback(7, 7, 20, &format!("d:{}", b.token)))
        .await
        .unwrap();
    pump(&mut app, &mut c, |a| a.sessions.entries[&2].turn.is_none()).await;
    let mut outputs = Vec::new();
    while let Some(job) = app.outgoing.next_job().unwrap() {
        outputs.push(job.message.text);
        app.outgoing.delete(job.id).unwrap();
    }
    assert!(
        outputs
            .iter()
            .any(|s| s.contains("#1: Decision:") && s.contains("accept"))
    );
    assert!(
        outputs
            .iter()
            .any(|s| s.contains("#2: Decision:") && s.contains("\"permissions\":{}"))
    );
    c.shutdown().await;
}
#[tokio::test]
async fn file_approval_cancel_restart_and_resume_preserve_session_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, mut c) = setup(dir.path()).await;
    app.update(message(7, "/new")).await.unwrap();
    app.update(message(7, "approval:file")).await.unwrap();
    pump(&mut app, &mut c, |a| !a.approvals.pending.is_empty()).await;
    assert!(
        app.approvals
            .pending
            .values()
            .next()
            .unwrap()
            .details
            .contains("+test")
    );
    app.update(message(7, "/cancel")).await.unwrap();
    pump(&mut app, &mut c, |a| a.sessions.entries[&1].turn.is_none()).await;
    assert!(app.approvals.pending.is_empty());
    app.update(message(7, "hang")).await.unwrap();
    assert!(app.sessions.entries[&1].turn.is_some());
    app.outgoing.save_sessions(&app.sessions).unwrap();
    c.shutdown().await;
    drop(app);
    let (mut app, mut c) = setup(dir.path()).await;
    assert!(!app.sessions.entries[&1].loaded);
    assert!(app.sessions.entries[&1].turn.is_none());
    app.update(message(8, "/switch 1")).await.unwrap();
    assert!(app.sessions.current(8, 8).is_err());
    app.update(message(7, "/resume 1")).await.unwrap();
    assert_eq!(app.sessions.current(7, 7).unwrap().thread, "thread-1");
    app.update(message(7, "continued prompt")).await.unwrap();
    pump(&mut app, &mut c, |a| a.sessions.entries[&1].turn.is_none()).await;
    assert_eq!(app.sessions.entries[&1].status, "completed");
    c.shutdown().await;
}
#[tokio::test]
async fn child_crash_invalidates_old_requests_without_replaying_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, mut c) = setup(dir.path()).await;
    app.update(message(7, "/new")).await.unwrap();
    app.update(message(7, "crash")).await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), c.events.recv())
        .await
        .unwrap();
    assert!(matches!(event, Some(Event::Disconnected) | None));
    app.disconnected().unwrap();
    assert!(app.sessions.entries[&1].turn.is_none());
    assert!(app.sessions.entries[&1].status.contains("unknown"));
    assert!(app.approvals.pending.is_empty());
    c.shutdown().await;
}

#[test]
fn telegram_rate_limit_policy() {
    use teloxide::{RequestError, types::Seconds};
    let rate = RequestError::RetryAfter(Seconds::from_seconds(19));
    assert_eq!(telegram_codex_bridge::telegram::retry_delay(&rate, 1), 19);
    assert!(telegram_codex_bridge::telegram::retryable(&rate));
    let invalid = RequestError::Api(teloxide::ApiError::InvalidToken);
    assert!(!telegram_codex_bridge::telegram::retryable(&invalid));
}

#[tokio::test]
async fn empty_session_recovery_expiry_and_turn_watchdog() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, c) = setup(dir.path()).await;
    app.update(message(7, "/new")).await.unwrap();
    assert!(app.sessions.entries[&1].empty);
    c.shutdown().await;
    drop(app);
    let (mut app, mut c) = setup(dir.path()).await;
    app.update(message(7, "/resume 1")).await.unwrap();
    assert!(app.sessions.entries[&1].loaded);
    app.update(message(7, "approval:command")).await.unwrap();
    pump(&mut app, &mut c, |a| !a.approvals.pending.is_empty()).await;
    for a in app.approvals.pending.values_mut() {
        a.deadline = std::time::Instant::now() - Duration::from_secs(1);
    }
    app.tick().await.unwrap();
    assert!(app.approvals.pending.is_empty());
    pump(&mut app, &mut c, |a| a.sessions.entries[&1].turn.is_none()).await;
    app.update(message(7, "hang")).await.unwrap();
    app.sessions.entries.get_mut(&1).unwrap().started =
        Some(telegram_codex_bridge::store::unix_now() - 3601);
    app.tick().await.unwrap();
    pump(&mut app, &mut c, |a| a.sessions.entries[&1].turn.is_none()).await;
    assert_eq!(app.sessions.entries[&1].status, "interrupted");
    c.shutdown().await;
}
