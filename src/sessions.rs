use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: u64,
    pub owner: u64,
    pub chat: i64,
    pub project: String,
    pub cwd: PathBuf,
    pub thread: String,
    pub turn: Option<String>,
    pub loaded: bool,
    pub status: String,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Sessions {
    pub entries: BTreeMap<u64, Session>,
    pub selected: HashMap<u64, u64>,
    pub projects: HashMap<u64, String>,
    next: u64,
}
impl Sessions {
    pub fn insert(
        &mut self,
        owner: u64,
        chat: i64,
        project: String,
        cwd: PathBuf,
        thread: String,
    ) -> Result<u64> {
        ensure!(self.entries.len() < 256, "Session limit reached (256)");
        self.next += 1;
        let id = self.next;
        self.entries.insert(
            id,
            Session {
                id,
                owner,
                chat,
                project,
                cwd,
                thread,
                turn: None,
                loaded: true,
                status: "ready".into(),
            },
        );
        self.selected.insert(owner, id);
        Ok(id)
    }
    pub fn owned(&self, id: u64, owner: u64, chat: i64) -> Result<&Session> {
        self.entries
            .get(&id)
            .filter(|s| s.owner == owner && s.chat == chat)
            .context("Session not found in this chat")
    }
    pub fn current(&self, owner: u64, chat: i64) -> Result<&Session> {
        self.owned(
            *self
                .selected
                .get(&owner)
                .context("Create or select a session first")?,
            owner,
            chat,
        )
    }
    pub fn select(&mut self, id: u64, owner: u64, chat: i64) -> Result<()> {
        self.owned(id, owner, chat)?;
        self.selected.insert(owner, id);
        Ok(())
    }
    pub fn by_thread(&self, thread: &str) -> Option<&Session> {
        self.entries.values().find(|s| s.thread == thread)
    }
    pub fn disconnected(&mut self) {
        for session in self.entries.values_mut() {
            session.loaded = false;
            if session.turn.take().is_some() || session.status == "starting" {
                session.status = "interrupted; outcome unknown; prompt not replayed".into();
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_switching_and_crash_state() {
        let mut s = Sessions::default();
        let a = s
            .insert(1, 1, "a".into(), "/a".into(), "thread-a".into())
            .unwrap();
        let b = s
            .insert(2, 2, "b".into(), "/b".into(), "thread-b".into())
            .unwrap();
        assert!(s.select(b, 1, 1).is_err());
        assert!(s.owned(a, 1, 2).is_err());
        assert_eq!(s.current(1, 1).unwrap().thread, "thread-a");
        s.entries.get_mut(&a).unwrap().turn = Some("turn-a".into());
        s.disconnected();
        assert!(s.entries[&a].turn.is_none());
        assert!(!s.entries[&b].loaded);
    }
}
