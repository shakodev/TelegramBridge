use crate::store::Store;
use crate::{
    approvals::Approvals,
    config::Config,
    messaging::{self, Command},
    progress::Progress,
    rpc::Rpc,
    sessions::Sessions,
    telegram::Outgoing,
    wire::Envelope,
    wire::ServerMethod,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use teloxide::types::{Update, UpdateKind};

pub struct App {
    pub config: Config,
    pub sessions: Sessions,
    pub rpc: Rpc,
    pub outgoing: Store,
    pub approvals: Approvals,
    pub progress: Progress,
}
impl App {
    pub fn say(&self, chat: i64, text: impl Into<String>) -> Result<()> {
        self.outgoing
            .enqueue(Outgoing {
                chat,
                text: text.into(),
                ..Default::default()
            })
            .context("Telegram output queue is full")
    }
    pub async fn update(&mut self, update: Update) -> Result<()> {
        if let UpdateKind::CallbackQuery(query) = &update.kind {
            return self.callback(query).await;
        }
        if let UpdateKind::Message(message) = update.kind {
            let Some(user) = message.from.as_ref() else {
                return Ok(());
            };
            if user.is_bot
                || !self
                    .config
                    .authorized(user.id.0, message.chat.id.0, message.chat.is_private())
            {
                return Ok(());
            }
            let Some(text) = message.text() else {
                return self.say(message.chat.id.0, "Only text prompts are supported");
            };
            if let Err(error) = self.command(user.id.0, message.chat.id.0, text).await {
                self.say(
                    message.chat.id.0,
                    format!("Error: {}", messaging::preview(&error.to_string(), 1500)),
                )?;
            }
        }
        Ok(())
    }
    pub async fn command(&mut self, user: u64, chat: i64, text: &str) -> Result<()> {
        match messaging::parse(text)? {
            Command::Help => self.say(chat, messaging::HELP)?,
            Command::Projects => self.say(
                chat,
                self.config
                    .projects
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?,
            Command::Project(alias) => {
                self.config.project(&alias)?;
                self.sessions.projects.insert(user, alias.clone());
                self.say(chat, format!("Selected project {alias} for new sessions"))?;
            }
            Command::New(alias) => {
                ensure!(
                    self.sessions.entries.len() < 256,
                    "Session limit reached (256)"
                );
                let alias = alias
                    .or_else(|| self.sessions.projects.get(&user).cloned())
                    .or_else(|| self.config.projects.keys().next().cloned())
                    .unwrap();
                let path = self.config.project(&alias)?.to_owned();
                let response = self
                    .rpc
                    .call("thread/start", self.config.thread_params(&path))
                    .await?;
                if let Err(e) = self.config.validate_thread(&response, &path) {
                    self.rpc.stop();
                    return Err(e);
                }
                let thread = response["thread"]["id"]
                    .as_str()
                    .context("Missing thread ID")?
                    .to_owned();
                let id = self
                    .sessions
                    .insert(user, chat, alias.clone(), path, thread)?;
                self.say(
                    chat,
                    format!("Session #{id} ({alias}) ready. Send a prompt."),
                )?;
            }
            Command::Sessions => {
                let list = self
                    .sessions
                    .entries
                    .values()
                    .filter(|s| s.owner == user && s.chat == chat)
                    .map(|s| format!("#{} [{}] {}", s.id, s.project, s.status))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.say(
                    chat,
                    if list.is_empty() {
                        "No sessions. Use /new".into()
                    } else {
                        list
                    },
                )?;
            }
            Command::Switch(id) | Command::Resume(id) => {
                self.sessions.owned(id, user, chat)?;
                self.load(id).await?;
                self.sessions.select(id, user, chat)?;
                self.say(chat, format!("Selected session #{id}"))?;
            }
            Command::Status => {
                let s = self.sessions.current(user, chat)?;
                self.say(chat, format!("#{} [{}] {}", s.id, s.project, s.status))?;
            }
            Command::Cancel(id) => {
                let s = match id {
                    Some(id) => self.sessions.owned(id, user, chat)?,
                    None => self.sessions.current(user, chat)?,
                }
                .clone();
                let turn = s.turn.context("Session has no active turn")?;
                let pending = self.approvals.remove_where(|a| a.session == s.id);
                self.deny(pending).await?;
                self.rpc
                    .call("turn/interrupt", json!({"threadId":s.thread,"turnId":turn}))
                    .await?;
                self.say(chat, format!("Interrupt requested for #{}", s.id))?;
            }
            Command::Prompt(text) => {
                let id = self.sessions.current(user, chat)?.id;
                self.load(id).await?;
                let s = self.sessions.entries[&id].clone();
                ensure!(
                    s.turn.is_none() && s.status != "starting",
                    "Session is busy; /cancel or switch sessions"
                );
                self.config.project(&s.project)?;
                self.sessions.entries.get_mut(&id).unwrap().status = "starting".into();
                self.sessions.entries.get_mut(&id).unwrap().empty = false;
                self.sessions.entries.get_mut(&id).unwrap().started =
                    Some(crate::store::unix_now());
                self.outgoing.save_sessions(&self.sessions)?;
                let response = self.rpc.call("turn/start",json!({"threadId":s.thread,"cwd":s.cwd,"input":[{"type":"text","text":text,"text_elements":[]}],"approvalPolicy":"untrusted","approvalsReviewer":"user","sandboxPolicy":self.config.sandbox_policy(&s.cwd)})).await;
                match response {
                    Ok(v) => {
                        let turn = v["turn"]["id"]
                            .as_str()
                            .context("Missing turn ID")?
                            .to_owned();
                        let s = self.sessions.entries.get_mut(&id).unwrap();
                        s.turn = Some(turn);
                        s.status = "running".into();
                        self.say(chat, format!("#{id}: working…"))?;
                    }
                    Err(e) => {
                        self.rpc.stop();
                        self.sessions.entries.get_mut(&id).unwrap().status =
                            "start failed; outcome may be unknown".into();
                        return Err(e);
                    }
                }
            }
        }
        self.outgoing.save_sessions(&self.sessions)?;
        Ok(())
    }
    pub async fn load(&mut self, id: u64) -> Result<()> {
        let s = self.sessions.entries[&id].clone();
        let path = self.config.project(&s.project)?;
        ensure!(
            s.cwd == path,
            "Saved workspace no longer matches configured project"
        );
        if !s.loaded {
            let mut params = self.config.thread_params(path);
            if !s.empty {
                params["threadId"] = json!(s.thread);
            }
            let response = self
                .rpc
                .call(
                    if s.empty {
                        "thread/start"
                    } else {
                        "thread/resume"
                    },
                    params,
                )
                .await?;
            if let Err(e) = self.config.validate_thread(&response, path) {
                self.rpc.stop();
                return Err(e);
            }
            if s.empty {
                self.sessions.entries.get_mut(&id).unwrap().thread = response["thread"]["id"]
                    .as_str()
                    .context("Missing replacement thread ID")?
                    .into();
            } else {
                ensure!(
                    response["thread"]["id"] == s.thread,
                    "Unexpected resumed thread"
                );
            }
            self.sessions.entries.get_mut(&id).unwrap().loaded = true;
        }
        Ok(())
    }
    pub async fn event(&mut self, message: Envelope) -> Result<()> {
        if let Some(id) = message.id {
            let method = message
                .method
                .as_deref()
                .and_then(|m| serde_json::from_value::<ServerMethod>(json!(m)).ok());
            let s = message.params["threadId"]
                .as_str()
                .and_then(|t| self.sessions.by_thread(t))
                .cloned();
            if let (Some(method), Some(s)) = (method, s) {
                let item = message.params["itemId"]
                    .as_str()
                    .and_then(|i| self.progress.items.get(&(s.thread.clone(), i.into())));
                match self.approvals.create(
                    id.clone(),
                    method,
                    message.params,
                    &s,
                    item,
                    self.config.approval_ttl,
                ) {
                    Ok(a) => {
                        self.outgoing.enqueue(Outgoing {
                            chat: s.chat,
                            text: a.details,
                            approval: Some(a.token),
                            expires: Some(
                                crate::store::unix_now()
                                    + self.config.approval_ttl.as_secs() as i64,
                            ),
                            ..Default::default()
                        })?;
                    }
                    Err(e) => {
                        let response = match method {
                            ServerMethod::ItemPermissionsRequestApproval => {
                                Some(json!({"permissions":{},"scope":"turn"}))
                            }
                            ServerMethod::ItemCommandExecutionRequestApproval
                            | ServerMethod::ItemFileChangeRequestApproval => {
                                Some(json!({"decision":"decline"}))
                            }
                            _ => None,
                        };
                        if let Some(result) = response {
                            self.rpc.respond(id, result).await?;
                        } else {
                            self.rpc.reject(id).await?;
                        }
                        self.say(s.chat, format!("#{}: request denied: {}", s.id, e))?;
                    }
                }
            } else {
                self.rpc.reject(id).await?;
            }
            return Ok(());
        }
        let p = message.params;
        let Some(s) = p["threadId"]
            .as_str()
            .and_then(|t| self.sessions.by_thread(t))
            .cloned()
        else {
            return Ok(());
        };
        if message.method.as_deref() == Some("item/completed")
            && let Some(item_id) = p["item"]["id"].as_str()
        {
            self.progress
                .items
                .remove(&(s.thread.clone(), item_id.into()));
            let resolved = self
                .approvals
                .remove_where(|a| a.thread == s.thread && a.params["itemId"] == item_id);
            for a in resolved {
                self.clear_button(&a)?;
            }
        }
        match message.method.as_deref() {
            Some("serverRequest/resolved") => {
                if let Ok(id) = serde_json::from_value::<crate::wire::Id>(p["requestId"].clone()) {
                    let removed = self
                        .approvals
                        .remove_where(|a| a.thread == s.thread && a.request == id);
                    for a in removed {
                        self.clear_button(&a)?;
                    }
                }
            }
            Some("item/agentMessage/delta")
                if s.turn.is_some() && p["turnId"].as_str() == s.turn.as_deref() =>
            {
                if let Some(delta) = p["delta"].as_str() {
                    self.progress.delta(s.id, delta);
                }
            }
            Some("item/started")
                if s.turn.is_some() && p["turnId"].as_str() == s.turn.as_deref() =>
            {
                self.progress.item(&s.thread, p["item"].clone());
                if let Some(kind) = p["item"]["type"].as_str() {
                    self.progress.delta(s.id, &format!("\n{kind}…\n"));
                }
            }
            Some("item/completed") if p["item"]["type"] == "agentMessage" => {
                if p["turnId"].as_str() == s.turn.as_deref()
                    && let Some(text) = p["item"]["text"].as_str()
                {
                    self.say(s.chat, format!("#{}: {}", s.id, text))?;
                }
            }
            Some("turn/completed")
                if s.turn.is_some() && p["turn"]["id"].as_str() == s.turn.as_deref() =>
            {
                let pending = self.approvals.remove_where(|a| a.session == s.id);
                for a in pending {
                    self.clear_button(&a)?;
                }
                self.progress.clear(&s.thread, s.id);
                let status = p["turn"]["status"].as_str().unwrap_or("unknown");
                let _ = self.outgoing.enqueue(Outgoing {
                    chat: s.chat,
                    text: format!("#{}: {status}", s.id),
                    progress: Some(format!("{}:{}", s.id, s.turn.as_deref().unwrap())),
                    expires: Some(crate::store::unix_now() + 60),
                    ..Default::default()
                });
                self.say(
                    s.chat,
                    format!(
                        "#{}: {status}{}",
                        s.id,
                        if p["turn"]["error"].is_null() {
                            String::new()
                        } else {
                            format!(
                                "\n{}",
                                messaging::preview(&p["turn"]["error"].to_string(), 2000)
                            )
                        }
                    ),
                )?;
                let session = self.sessions.entries.get_mut(&s.id).unwrap();
                session.turn = None;
                session.started = None;
                session.status = status.into();
                self.outgoing.save_sessions(&self.sessions)?;
            }
            Some("error") => self.say(
                s.chat,
                format!(
                    "#{}: Codex error: {}",
                    s.id,
                    messaging::preview(&p["error"].to_string(), 2000)
                ),
            )?,
            _ => {}
        }
        Ok(())
    }
    fn clear_button(&self, a: &crate::approvals::Approval) -> Result<()> {
        if let Some(message) = a.message {
            self.outgoing.enqueue(Outgoing {
                chat: a.chat,
                clear: Some(message),
                ..Default::default()
            })?;
        }
        Ok(())
    }
    async fn deny(&self, approvals: Vec<crate::approvals::Approval>) -> Result<()> {
        for a in approvals {
            self.rpc.respond(a.request.clone(), a.result(false)).await?;
            self.clear_button(&a)?;
        }
        Ok(())
    }
    pub async fn receipt(&mut self, receipt: crate::telegram::Receipt) -> Result<()> {
        if let Some(message) = receipt.message {
            self.approvals.bind(&receipt.token, message);
        } else if let Some(a) = self.approvals.pending.remove(&receipt.token) {
            self.deny(vec![a]).await?;
        }
        Ok(())
    }
    async fn callback(&mut self, query: &teloxide::types::CallbackQuery) -> Result<()> {
        let Some(message) = query.regular_message() else {
            return Ok(());
        };
        if query.from.is_bot
            || !self.config.authorized(
                query.from.id.0,
                message.chat.id.0,
                message.chat.is_private(),
            )
        {
            return Ok(());
        }
        let data = query.data.as_deref().unwrap_or("");
        let turn = data
            .split_once(':')
            .and_then(|(_, token)| self.approvals.pending.get(token))
            .and_then(|a| self.sessions.entries.get(&a.session))
            .and_then(|s| s.turn.clone());
        let result = self.approvals.consume(
            data,
            query.from.id.0,
            message.chat.id.0,
            message.id.0,
            turn.as_deref(),
            std::time::Instant::now(),
        );
        let text = match result {
            Ok((a, accept)) => {
                self.rpc
                    .respond(a.request.clone(), a.result(accept))
                    .await?;
                self.clear_button(&a)?;
                if accept {
                    "Approved this action"
                } else {
                    "Denied"
                }
                .to_owned()
            }
            Err(_) => "Invalid, expired, or already answered approval".into(),
        };
        self.outgoing.enqueue(Outgoing {
            chat: message.chat.id.0,
            text,
            callback: Some(query.id.0.clone()),
            expires: Some(crate::store::unix_now() + 20),
            ..Default::default()
        })?;
        Ok(())
    }
    pub async fn tick(&mut self) -> Result<()> {
        let overdue: Vec<_> = self
            .sessions
            .entries
            .values()
            .filter(|s| {
                s.turn.is_some()
                    && s.started.is_some_and(|t| {
                        crate::store::unix_now() - t
                            >= if s.status == "timeout" { 30 } else { 3600 }
                    })
            })
            .cloned()
            .collect();
        for s in overdue {
            if s.status == "timeout" {
                self.rpc.stop();
                return Ok(());
            }
            self.rpc
                .call(
                    "turn/interrupt",
                    json!({"threadId":s.thread,"turnId":s.turn}),
                )
                .await?;
            let state = self.sessions.entries.get_mut(&s.id).unwrap();
            state.status = "timeout".into();
            state.started = Some(crate::store::unix_now());
            self.say(
                s.chat,
                format!(
                    "#{}: one-hour turn limit reached; interrupt requested",
                    s.id
                ),
            )?;
        }
        let now = std::time::Instant::now();
        let expired = self.approvals.remove_where(|a| a.deadline <= now);
        for a in &expired {
            self.say(
                a.chat,
                format!("#{}: approval expired and was denied", a.session),
            )?;
        }
        self.deny(expired).await?;
        for (id, text) in self.progress.drain() {
            if let Some(s) = self.sessions.entries.get(&id)
                && let Some(turn) = &s.turn
            {
                // Progress may be dropped under pressure; final output may not.
                let _ = self.outgoing.enqueue(Outgoing {
                    chat: s.chat,
                    text,
                    progress: Some(format!("{id}:{turn}")),
                    expires: Some(crate::store::unix_now() + 30),
                    ..Default::default()
                });
            }
        }
        Ok(())
    }
    pub fn disconnected(&mut self) -> Result<()> {
        self.approvals.pending.clear();
        self.progress = Default::default();
        self.outgoing.invalidate_transient()?;
        for s in self.sessions.entries.values() {
            if s.turn.is_some() || s.status == "starting" {
                self.say(s.chat,format!("#{}: Codex disconnected. Turn outcome may be unknown. Prompts and approvals were not replayed. Resume with /resume {}.",s.id,s.id))?;
            }
        }
        self.sessions.disconnected();
        self.outgoing.save_sessions(&self.sessions)?;
        Ok(())
    }
}
