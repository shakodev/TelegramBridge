//! Deterministic external process used by integration tests; never shipped by default.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
};
fn emit(value: Value) {
    let mut out = io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
fn complete(thread: &str, turn: &str, text: &str, status: &str) {
    emit(
        json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":turn,"itemId":"answer","delta":"Live test progress"}}),
    );
    emit(
        json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"id":"answer","type":"agentMessage","text":text}}}),
    );
    emit(
        json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"status":status,"error":null}}}),
    );
}
fn main() {
    let mut threads = HashMap::<String, String>::new();
    let mut pending = HashMap::<String, (String, String)>::new();
    let mut sequence = 0;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let v: Value = serde_json::from_str(&line).unwrap();
        let p = &v["params"];
        let id = &v["id"];
        match v["method"].as_str() {
            Some("initialize") => emit(json!({"id":id,"result":{"userAgent":"mock"}})),
            Some("initialized") => {}
            Some("thread/start" | "thread/resume") => {
                let thread = if v["method"] == "thread/start" {
                    format!("thread-{}", threads.len() + 1)
                } else {
                    p["threadId"].as_str().unwrap().into()
                };
                let cwd = p["cwd"].as_str().unwrap().to_owned();
                threads.insert(thread.clone(), cwd.clone());
                emit(
                    json!({"id":id,"result":{"thread":{"id":thread},"cwd":cwd,"approvalPolicy":"untrusted","approvalsReviewer":"user","sandbox":{"type":"readOnly","networkAccess":false}}}),
                );
            }
            Some("turn/start") => {
                sequence += 1;
                let thread = p["threadId"].as_str().unwrap();
                let turn = format!("turn-{sequence}");
                let text = p["input"][0]["text"].as_str().unwrap();
                emit(json!({"id":id,"result":{"turn":{"id":turn,"status":"inProgress"}}}));
                if text == "crash" {
                    std::process::exit(17);
                }
                if text == "hang" {
                    continue;
                }
                if text.starts_with("approval:") {
                    let method = match text {
                        "approval:file" => "item/fileChange/requestApproval",
                        "approval:permissions" => "item/permissions/requestApproval",
                        _ => "item/commandExecution/requestApproval",
                    };
                    let item = format!("item-{sequence}");
                    let request = format!("request-{sequence}");
                    let cwd = &threads[thread];
                    emit(
                        json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"item":{"id":item,"type":"fileChange","changes":[{"path":std::path::Path::new(cwd).join("new.txt"),"kind":{"type":"add"},"diff":"+test\n"}]}}}),
                    );
                    emit(
                        json!({"id":request,"method":method,"params":{"threadId":thread,"turnId":turn,"itemId":item,"cwd":cwd,"command":"echo hello","permissions":{"network":{"enabled":true}}}}),
                    );
                    pending.insert(request, (thread.into(), turn));
                } else {
                    complete(
                        thread,
                        &turn,
                        &format!("Answer for {thread}: {text}"),
                        "completed",
                    );
                }
            }
            Some("turn/interrupt") => {
                emit(json!({"id":id,"result":{}}));
                complete(
                    p["threadId"].as_str().unwrap(),
                    p["turnId"].as_str().unwrap(),
                    "Cancelled",
                    "interrupted",
                );
            }
            Some(_) => emit(json!({"id":id,"error":{"code":-32601,"message":"unknown"}})),
            None => {
                if let Some((thread, turn)) = id.as_str().and_then(|r| pending.remove(r)) {
                    emit(
                        json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":id}}),
                    );
                    complete(
                        &thread,
                        &turn,
                        &format!("Decision: {}", v["result"]),
                        "completed",
                    );
                }
            }
        }
    }
}
