//! Approval capabilities exist only in memory and are consumed before replying.
//! A process restart invalidates every old button, even if Codex reuses an ID.
use crate::{
    sessions::Session,
    wire::{Id, ServerMethod},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Component, Path},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Approval {
    pub token: String,
    pub request: Id,
    pub method: ServerMethod,
    pub session: u64,
    pub owner: u64,
    pub chat: i64,
    pub thread: String,
    pub turn: String,
    pub message: Option<i32>,
    pub deadline: Instant,
    pub params: Value,
    pub details: String,
}
impl Approval {
    pub fn result(&self, accept: bool) -> Value {
        match self.method {
            ServerMethod::ItemPermissionsRequestApproval => {
                json!({"permissions":if accept {self.params["permissions"].clone()} else {json!({})},"scope":"turn"})
            }
            _ => json!({"decision":if accept {"accept"} else {"decline"}}),
        }
    }
}
#[derive(Default)]
pub struct Approvals {
    pub pending: HashMap<String, Approval>,
}
impl Approvals {
    pub fn create(
        &mut self,
        id: Id,
        method: ServerMethod,
        params: Value,
        session: &Session,
        item: Option<&Value>,
        ttl: Duration,
    ) -> Result<Approval> {
        ensure!(self.pending.len() < 128, "Too many pending approvals");
        ensure!(
            !self.pending.values().any(|a| a.request == id),
            "Duplicate server request"
        );
        ensure!(
            params["threadId"] == session.thread
                && params["turnId"].as_str() == session.turn.as_deref()
                && session.turn.is_some(),
            "Stale approval turn"
        );
        ensure!(
            params["itemId"].as_str().is_some_and(|s| !s.is_empty()),
            "Missing item ID"
        );
        ensure!(
            params["environmentId"].is_null(),
            "Remote execution environments are not supported"
        );
        if let Some(cwd) = params["cwd"].as_str() {
            ensure!(
                Path::new(cwd).canonicalize()? == session.cwd,
                "Approval cwd differs from workspace"
            );
        }
        match method {
            ServerMethod::ItemCommandExecutionRequestApproval => {
                ensure!(
                    params["command"].as_str().is_some_and(|s| !s.is_empty())
                        || params["networkApprovalContext"].is_object(),
                    "Missing action details"
                );
                ensure!(params["cwd"].is_string(), "Missing command cwd");
                if let Some(decisions) = params["availableDecisions"].as_array() {
                    ensure!(
                        decisions.contains(&json!("accept")),
                        "Single-action approval not offered"
                    );
                }
                if !params["additionalPermissions"].is_null() {
                    validate_permissions(&params["additionalPermissions"], &session.cwd)?;
                }
            }
            ServerMethod::ItemFileChangeRequestApproval => {
                ensure!(
                    params["grantRoot"].is_null(),
                    "Session-wide write grants are not supported"
                );
                let item = item.context("File change details are missing")?;
                let changes = item["changes"]
                    .as_array()
                    .context("File changes are missing")?;
                ensure!(!changes.is_empty(), "File changes are empty");
                for change in changes {
                    validate_path(
                        change["path"].as_str().context("Missing changed path")?,
                        &session.cwd,
                    )?;
                }
            }
            ServerMethod::ItemPermissionsRequestApproval => {
                validate_permissions(&params["permissions"], &session.cwd)?
            }
            _ => bail!("Unsupported interactive request"),
        }
        let details = format!(
            "Session #{} [{}]\nApproval requested (expires in {}s). Approve this action only.\n{}\n{}{}",
            session.id,
            session.project,
            ttl.as_secs(),
            serde_json::to_string(&method)?,
            serde_json::to_string_pretty(&params)?,
            item.map(|v| format!(
                "\nItem details:\n{}",
                serde_json::to_string_pretty(v).unwrap()
            ))
            .unwrap_or_default()
        );
        ensure!(
            details.encode_utf16().count() <= 12_000,
            "Action details exceed safe Telegram review limit"
        );
        let token = uuid::Uuid::new_v4().simple().to_string();
        let approval = Approval {
            token: token.clone(),
            request: id,
            method,
            session: session.id,
            owner: session.owner,
            chat: session.chat,
            thread: session.thread.clone(),
            turn: session.turn.clone().unwrap(),
            message: None,
            deadline: Instant::now() + ttl,
            params,
            details,
        };
        self.pending.insert(token, approval.clone());
        Ok(approval)
    }
    pub fn bind(&mut self, token: &str, message: i32) {
        if let Some(a) = self.pending.get_mut(token) {
            a.message = Some(message);
        }
    }
    pub fn consume(
        &mut self,
        data: &str,
        user: u64,
        chat: i64,
        message: i32,
        turn: Option<&str>,
        now: Instant,
    ) -> Result<(Approval, bool)> {
        let (choice, token) = data.split_once(':').context("Invalid approval callback")?;
        ensure!(
            matches!(choice, "a" | "d") && token.len() == 32,
            "Invalid approval callback"
        );
        let a = self
            .pending
            .get(token)
            .context("Approval expired, already answered, or invalidated by restart")?;
        ensure!(
            a.owner == user && a.chat == chat && a.message == Some(message),
            "Approval belongs to another user, chat, or message"
        );
        ensure!(
            now < a.deadline && turn == Some(a.turn.as_str()),
            "Approval is stale or expired"
        );
        Ok((self.pending.remove(token).unwrap(), choice == "a"))
    }
    pub fn remove_where(&mut self, mut predicate: impl FnMut(&Approval) -> bool) -> Vec<Approval> {
        let keys: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, a)| predicate(a))
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter()
            .filter_map(|key| self.pending.remove(&key))
            .collect()
    }
}
/// Resolve an existing ancestor for new files, rejecting traversal and symlink escapes.
pub fn validate_path(raw: &str, root: &Path) -> Result<()> {
    let path = Path::new(raw);
    ensure!(
        path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "Permission path must be absolute without parent traversal"
    );
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().context("Invalid permission path")?;
    }
    ensure!(
        existing.canonicalize()?.starts_with(root),
        "Requested path escapes configured workspace"
    );
    Ok(())
}
pub fn validate_permissions(value: &Value, root: &Path) -> Result<()> {
    let obj = value.as_object().context("Missing permission details")?;
    ensure!(
        obj.keys()
            .all(|k| matches!(k.as_str(), "fileSystem" | "network")),
        "Unknown permission category"
    );
    if let Some(network) = obj.get("network").filter(|v| !v.is_null()) {
        let network = network.as_object().context("Invalid network permissions")?;
        ensure!(
            network.keys().all(|k| k == "enabled")
                && network.get("enabled").is_some_and(Value::is_boolean),
            "Unknown network permission"
        );
    }
    if let Some(fs) = obj.get("fileSystem").filter(|v| !v.is_null()) {
        let fs = fs.as_object().context("Invalid filesystem permissions")?;
        ensure!(
            fs.keys().all(|k| matches!(
                k.as_str(),
                "read" | "write" | "entries" | "globScanMaxDepth"
            )),
            "Unknown filesystem permission"
        );
        for key in ["read", "write"] {
            if let Some(paths) = fs.get(key).filter(|v| !v.is_null()) {
                for path in paths.as_array().context("Invalid permission paths")? {
                    validate_path(path.as_str().context("Invalid path")?, root)?;
                }
            }
        }
        if let Some(entries) = fs.get("entries").filter(|v| !v.is_null()) {
            for entry in entries.as_array().context("Invalid filesystem entries")? {
                ensure!(
                    entry["path"]["type"] == "path"
                        && matches!(entry["access"].as_str(), Some("read" | "write" | "deny")),
                    "Only concrete filesystem paths are supported"
                );
                validate_path(
                    entry["path"]["path"]
                        .as_str()
                        .context("Invalid entry path")?,
                    root,
                )?;
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Session, Value) {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let p = json!({"threadId":"t","turnId":"v","itemId":"i","cwd":cwd,"command":"echo hi"});
        (
            dir,
            Session {
                id: 1,
                owner: 7,
                chat: 7,
                project: "p".into(),
                cwd,
                thread: "t".into(),
                turn: Some("v".into()),
                loaded: true,
                status: "running".into(),
                empty: false,
                started: None,
            },
            p,
        )
    }
    #[test]
    fn buttons_bind_owner_message_turn_and_are_one_time() {
        let (_dir, s, p) = fixture();
        let mut registry = Approvals::default();
        let a = registry
            .create(
                Id::Number(1),
                ServerMethod::ItemCommandExecutionRequestApproval,
                p,
                &s,
                None,
                Duration::from_secs(30),
            )
            .unwrap();
        let data = format!("a:{}", a.token);
        registry.bind(&a.token, 99);
        assert!(
            registry
                .consume(&data, 8, 7, 99, Some("v"), Instant::now())
                .is_err()
        );
        assert!(
            registry
                .consume(&data, 7, 8, 99, Some("v"), Instant::now())
                .is_err()
        );
        assert!(
            registry
                .consume(&data, 7, 7, 100, Some("v"), Instant::now())
                .is_err()
        );
        assert!(
            registry
                .consume(&data, 7, 7, 99, Some("other"), Instant::now())
                .is_err()
        );
        assert!(
            registry
                .consume(&data, 7, 7, 99, Some("v"), a.deadline)
                .is_err()
        );
        let (a, yes) = registry
            .consume(&data, 7, 7, 99, Some("v"), Instant::now())
            .unwrap();
        assert!(yes);
        assert_eq!(a.result(true), json!({"decision":"accept"}));
        assert!(
            registry
                .consume(&data, 7, 7, 99, Some("v"), Instant::now())
                .is_err()
        );
    }
    #[test]
    fn permissions_are_explicit_turn_scoped_and_denied_empty() {
        let (_dir, s, mut p) = fixture();
        let mut r = Approvals::default();
        p["permissions"] = json!({"network":{"enabled":true}});
        let a = r
            .create(
                Id::Text("r".into()),
                ServerMethod::ItemPermissionsRequestApproval,
                p,
                &s,
                None,
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(a.result(false), json!({"permissions":{},"scope":"turn"}));
        assert_eq!(a.result(true)["scope"], "turn");
        assert!(validate_permissions(&json!({"fileSystem":{"entries":[{"access":"write","path":{"type":"special","value":{"kind":"root"}}}]}}),&s.cwd).is_err());
        assert!(validate_path(s.cwd.parent().unwrap().to_str().unwrap(), &s.cwd).is_err());
        assert!(validate_path(s.cwd.join("new.txt").to_str().unwrap(), &s.cwd).is_ok());
    }
    #[test]
    fn missing_details_and_session_grants_fail_closed() {
        let (_dir, s, mut p) = fixture();
        let mut r = Approvals::default();
        assert!(
            r.create(
                Id::Number(1),
                ServerMethod::ItemFileChangeRequestApproval,
                p.clone(),
                &s,
                None,
                Duration::from_secs(2)
            )
            .is_err()
        );
        p["command"] = Value::Null;
        assert!(
            r.create(
                Id::Number(1),
                ServerMethod::ItemCommandExecutionRequestApproval,
                p,
                &s,
                None,
                Duration::from_secs(2)
            )
            .is_err()
        );
    }
}
