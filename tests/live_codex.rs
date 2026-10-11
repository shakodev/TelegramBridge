//! Explicit opt-in: real installed Codex, isolated home, loopback model fixture.
//! Does not use Telegram, user credentials, or a hosted model.
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use telegram_codex_bridge::{
    config::Config,
    rpc::{Connection, Event},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
#[ignore = "requires installed Codex; all model traffic goes to a loopback fixture"]
async fn installed_server_start_turn_resume_list_and_policy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let end = loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..end]);
        assert!(
            headers.starts_with("POST /v1/responses "),
            "Unexpected fixture request"
        );
        let length: usize = headers
            .lines()
            .find_map(|l| {
                l.to_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse().unwrap())
            })
            .unwrap();
        while bytes.len() < end + length {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
        }
        let item = json!({"id":"msg_fixture","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"BRIDGE_SMOKE_OK","annotations":[]}]});
        let events = [
            json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress","output":[]}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_fixture","type":"message","role":"assistant","content":[]}}),
            json!({"type":"response.output_text.delta","item_id":"msg_fixture","output_index":0,"content_index":0,"delta":"BRIDGE_SMOKE_OK"}),
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
        ];
        let body = events
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),format!("model = \"gpt-5.1-codex\"\nmodel_provider = \"fixture\"\n[model_providers.fixture]\nname = \"Loopback protocol fixture\"\nbase_url = \"http://{address}/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\nsupports_websockets = false\n")).unwrap();
    let cwd = workspace.path().canonicalize().unwrap();
    let executable = std::env::var_os("CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| "codex".into());
    let config = Config {
        token: String::new(),
        users: Default::default(),
        projects: Default::default(),
        codex: executable.clone(),
        database: "unused".into(),
        sandbox: "read-only".into(),
        rpc_timeout: Duration::from_secs(30),
        approval_ttl: Duration::from_secs(60),
    };
    let mut c =
        Connection::spawn_with_home(&executable, &cwd, config.rpc_timeout, Some(home.path()))
            .await
            .unwrap();
    let start = c
        .rpc
        .call("thread/start", config.thread_params(&cwd))
        .await
        .unwrap();
    config.validate_thread(&start, &cwd).unwrap();
    let thread = start["thread"]["id"].as_str().unwrap().to_owned();
    c.rpc.call("turn/start",json!({"threadId":thread,"input":[{"type":"text","text":"Return the fixture marker; do not call tools.","text_elements":[]}],"sandboxPolicy":config.sandbox_policy(&cwd),"approvalPolicy":"untrusted","approvalsReviewer":"user"})).await.unwrap();
    let mut saw_text = false;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let Some(Event::Message(m)) = c.events.recv().await else {
                panic!("Disconnected before completion")
            };
            if let Some(id) = m.id {
                c.rpc.reject(id).await.unwrap();
                continue;
            }
            if m.method.as_deref() == Some("item/completed")
                && m.params["item"]["text"] == "BRIDGE_SMOKE_OK"
            {
                saw_text = true;
            }
            if m.method.as_deref() == Some("turn/completed") {
                assert_eq!(m.params["turn"]["status"], "completed", "{}", m.params);
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_text);
    server.await.unwrap();
    c.shutdown().await;
    let c = Connection::spawn_with_home(&executable, &cwd, config.rpc_timeout, Some(home.path()))
        .await
        .unwrap();
    let mut params = config.thread_params(&cwd);
    params["threadId"] = json!(thread);
    let resumed = c.rpc.call("thread/resume", params).await.unwrap();
    config.validate_thread(&resumed, &cwd).unwrap();
    assert_eq!(resumed["thread"]["id"], thread);
    let listed = c
        .rpc
        .call(
            "thread/list",
            json!({"cwd":cwd,"limit":10,"modelProviders":[]}),
        )
        .await
        .unwrap();
    assert!(listed["data"].is_array());
    c.shutdown().await;
}
