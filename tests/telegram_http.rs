use serde_json::{Value, json};
use std::time::Duration;
use telegram_codex_bridge::{
    store::Store,
    telegram::{self, Outgoing},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

#[tokio::test]
async fn actual_teloxide_http_retries_429_and_places_buttons_after_all_details() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for index in 0..3 {
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
            bodies.push(serde_json::from_slice::<Value>(&bytes[end..end + length]).unwrap());
            let response=if index==0 {json!({"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":1}})} else {
                json!({"ok":true,"result":{"message_id":40+index,"date":1,"chat":{"id":7,"type":"private"},"text":"delivered"}})
            }.to_string();
            let status = if index == 0 {
                "429 Too Many Requests"
            } else {
                "200 OK"
            };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        }
        bodies
    });
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db")).unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    store
        .enqueue(Outgoing {
            chat: 7,
            text: "x".repeat(5000),
            approval: Some(token.into()),
            ..Default::default()
        })
        .unwrap();
    let bot =
        teloxide::Bot::new("123:test").set_api_url(format!("http://{address}").parse().unwrap());
    let (tx, mut rx) = mpsc::channel(4);
    let delivery = tokio::spawn(telegram::deliver(bot, store.clone(), tx, [7].into()));
    let receipt = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.token, token);
    assert_eq!(receipt.message, Some(42));
    delivery.abort();
    let _ = delivery.await;
    let bodies = server.await.unwrap();
    assert_eq!(bodies[0]["text"], bodies[1]["text"]);
    assert!(bodies[1]["reply_markup"].is_null());
    assert_eq!(
        bodies[2]["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
        format!("a:{token}")
    );
    assert!(store.next_job().unwrap().is_none());
}
