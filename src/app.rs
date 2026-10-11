use crate::{
    config::Config,
    messaging::{self, Command},
    rpc::Rpc,
    sessions::Sessions,
    telegram::Outgoing,
    wire::Envelope,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use teloxide::types::{Update, UpdateKind};
use tokio::sync::mpsc;

pub struct App {
    pub config: Config,
    pub sessions: Sessions,
    pub rpc: Rpc,
    pub outgoing: mpsc::Sender<Outgoing>,
}
impl App {
    pub fn say(&self, chat: i64, text: impl Into<String>) -> Result<()> {
        self.outgoing
            .try_send(Outgoing {
                chat,
                text: text.into(),
            })
            .context("Telegram output queue is full")
    }
    pub async fn update(&mut self, update: Update) -> Result<()> {
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
                        self.sessions.entries.get_mut(&id).unwrap().status =
                            "start failed; outcome may be unknown".into();
                        return Err(e);
                    }
                }
            }
        }
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
            params["threadId"] = json!(s.thread);
            let response = self.rpc.call("thread/resume", params).await?;
            if let Err(e) = self.config.validate_thread(&response, path) {
                self.rpc.stop();
                return Err(e);
            }
            ensure!(
                response["thread"]["id"] == s.thread,
                "Unexpected resumed thread"
            );
            self.sessions.entries.get_mut(&id).unwrap().loaded = true;
        }
        Ok(())
    }
    pub async fn event(&mut self, message: Envelope) -> Result<()> {
        if let Some(id) = message.id {
            self.rpc.reject(id).await?;
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
        match message.method.as_deref() {
            Some("item/completed") if p["item"]["type"] == "agentMessage" => {
                if p["turnId"].as_str() == s.turn.as_deref() {
                    if let Some(text) = p["item"]["text"].as_str() {
                        self.say(s.chat, format!("#{}: {}", s.id, text))?;
                    }
                }
            }
            Some("turn/completed") if p["turn"]["id"].as_str() == s.turn.as_deref() => {
                let status = p["turn"]["status"].as_str().unwrap_or("unknown");
                self.say(s.chat, format!("#{}: {status}", s.id))?;
                let session = self.sessions.entries.get_mut(&s.id).unwrap();
                session.turn = None;
                session.status = status.into();
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
}
