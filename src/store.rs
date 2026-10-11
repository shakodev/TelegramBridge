//! Bounded SQLite transactions. No database lock crosses an await.
use crate::{messaging::split_text, sessions::Sessions, telegram::Outgoing};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
struct Inner {
    db: Mutex<Connection>,
    _lock: File,
    notify: Notify,
}
#[derive(Clone)]
pub struct Store(Arc<Inner>);
pub struct Job {
    pub id: i64,
    pub message: Outgoing,
    pub part: usize,
    pub attempts: u32,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        lock.try_lock()
            .context("Another bridge is already using this database")?;
        let db = Connection::open(path)?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= 1,
            "Database was created by a newer bridge; refusing to downgrade"
        );
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-2048;
            CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS updates (id INTEGER PRIMARY KEY, claimed INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS outbox (id INTEGER PRIMARY KEY AUTOINCREMENT, chat INTEGER NOT NULL, body TEXT NOT NULL,
              part INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, ready INTEGER NOT NULL DEFAULT 0,
              transient INTEGER NOT NULL, coalesce TEXT, priority INTEGER NOT NULL, created INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS outbox_ready ON outbox(ready,priority,id);
            CREATE TABLE IF NOT EXISTS failed_delivery (id INTEGER PRIMARY KEY, body TEXT NOT NULL, failed INTEGER NOT NULL);
            PRAGMA user_version=1;")?;
        db.execute("DELETE FROM outbox WHERE transient=1", [])?;
        Ok(Self(Arc::new(Inner {
            db: Mutex::new(db),
            _lock: lock,
            notify: Notify::new(),
        })))
    }
    fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.0
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("Database lock poisoned"))
    }
    pub fn load_sessions(&self) -> Result<Sessions> {
        let value: Option<String> = self
            .db()?
            .query_row("SELECT value FROM state WHERE key='sessions'", [], |r| {
                r.get(0)
            })
            .optional()?;
        let mut sessions: Sessions = value
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or_default();
        sessions.disconnected();
        Ok(sessions)
    }
    pub fn save_sessions(&self, sessions: &Sessions) -> Result<()> {
        self.db()?.execute("INSERT INTO state VALUES('sessions',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(sessions)?])?;
        Ok(())
    }
    /// Commit before invoking a non-idempotent Codex RPC; ambiguous prompts never replay.
    pub fn claim_update(&self, id: u32) -> Result<bool> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        let fresh = tx.execute(
            "INSERT OR IGNORE INTO updates(id,claimed) VALUES(?1,?2)",
            params![id, unix_now()],
        )? == 1;
        tx.execute("INSERT INTO state VALUES('offset',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[(i64::from(id)+1).to_string()])?;
        tx.execute("DELETE FROM updates WHERE id NOT IN (SELECT id FROM updates ORDER BY claimed DESC,id DESC LIMIT 4096)",[])?;
        tx.commit()?;
        Ok(fresh)
    }
    pub fn offset(&self) -> Result<i32> {
        let value: Option<String> = self
            .db()?
            .query_row("SELECT value FROM state WHERE key='offset'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(value.map(|s| s.parse()).transpose()?.unwrap_or(0))
    }
    pub fn enqueue(&self, mut message: Outgoing) -> Result<()> {
        if message.text.len() > 128_000 {
            message.text = crate::messaging::preview(&message.text, 30_000);
        }
        let body = serde_json::to_string(&message)?;
        let transient = message.approval.is_some()
            || message.progress.is_some()
            || message.callback.is_some()
            || message.clear.is_some();
        let priority = if message.callback.is_some() || message.clear.is_some() {
            0
        } else if message.approval.is_some() {
            1
        } else if message.progress.is_some() {
            3
        } else {
            2
        };
        let mut db = self.db()?;
        let tx = db.transaction()?;
        if let Some(key) = &message.progress {
            tx.execute("DELETE FROM outbox WHERE coalesce=?1", [key])?;
        }
        let (count, size): (i64, i64) = tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(body)),0) FROM outbox",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            count < 1024 && size + (body.len() as i64) < 8 * 1024 * 1024,
            "Durable Telegram outbox is full; bridge stopped to protect output"
        );
        tx.execute("INSERT INTO outbox(chat,body,transient,coalesce,priority,created) VALUES(?1,?2,?3,?4,?5,?6)",params![message.chat,body,transient,message.progress,priority,unix_now()])?;
        tx.commit()?;
        self.0.notify.notify_one();
        Ok(())
    }
    pub fn next_job(&self) -> Result<Option<Job>> {
        let db = self.db()?;
        let row: Option<(i64,String,usize,u32)> = db.query_row(
            "SELECT a.id,a.body,a.part,a.attempts FROM outbox a WHERE a.ready<=?1 AND NOT EXISTS
              (SELECT 1 FROM outbox b WHERE b.chat=a.chat AND b.id<a.id AND b.priority<=a.priority)
             ORDER BY a.priority,a.id LIMIT 1",[unix_now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        row.map(|(id, body, part, attempts)| {
            Ok(Job {
                id,
                message: serde_json::from_str(&body)?,
                part,
                attempts,
            })
        })
        .transpose()
    }
    pub fn advance(&self, job: &Job) -> Result<bool> {
        let complete = job.message.callback.is_some()
            || job.message.clear.is_some()
            || job.part + 1 >= split_text(&job.message.text, 3800).len();
        if complete {
            self.delete(job.id)?;
        } else {
            self.db()?.execute(
                "UPDATE outbox SET part=part+1,attempts=0 WHERE id=?1",
                [job.id],
            )?;
        }
        Ok(complete)
    }
    pub fn delete(&self, id: i64) -> Result<()> {
        self.db()?.execute("DELETE FROM outbox WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn dead_letter(&self, id: i64) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        tx.execute("INSERT OR REPLACE INTO failed_delivery SELECT id,body,?2 FROM outbox WHERE id=?1 AND transient=0",params![id,unix_now()])?;
        tx.execute("DELETE FROM failed_delivery WHERE id NOT IN (SELECT id FROM failed_delivery ORDER BY failed DESC,id DESC LIMIT 64)",[])?;
        tx.execute("DELETE FROM outbox WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn retry(&self, id: i64, seconds: u64) -> Result<()> {
        self.db()?.execute(
            "UPDATE outbox SET attempts=attempts+1,ready=?2 WHERE id=?1",
            params![
                id,
                unix_now().saturating_add(seconds.min(i64::MAX as u64) as i64)
            ],
        )?;
        Ok(())
    }
    pub fn invalidate_transient(&self) -> Result<()> {
        self.db()?
            .execute("DELETE FROM outbox WHERE transient=1", [])?;
        Ok(())
    }
    pub fn next_wait(&self) -> Result<Option<u64>> {
        let earliest: Option<i64> =
            self.db()?
                .query_row("SELECT MIN(a.ready) FROM outbox a WHERE NOT EXISTS (SELECT 1 FROM outbox b WHERE b.chat=a.chat AND b.id<a.id AND b.priority<=a.priority)", [], |r| r.get(0))?;
        Ok(earliest.map(|t| (t - unix_now()).max(1) as u64))
    }
    pub async fn notified(&self) {
        self.0.notify.notified().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reopen_preserves_context_and_dedup_but_invalidates_buttons() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        {
            let db = Store::open(&path).unwrap();
            assert!(Store::open(&path).is_err());
            let mut sessions = Sessions::default();
            let id = sessions
                .insert(7, 7, "p".into(), dir.path().into(), "t".into())
                .unwrap();
            sessions.entries.get_mut(&id).unwrap().turn = Some("v".into());
            db.save_sessions(&sessions).unwrap();
            assert!(db.claim_update(5).unwrap());
            assert!(!db.claim_update(5).unwrap());
            db.enqueue(Outgoing {
                chat: 7,
                text: "final".into(),
                ..Default::default()
            })
            .unwrap();
            db.enqueue(Outgoing {
                chat: 7,
                text: "button".into(),
                approval: Some("token".into()),
                ..Default::default()
            })
            .unwrap();
        }
        let db = Store::open(&path).unwrap();
        let s = db.load_sessions().unwrap();
        assert_eq!(s.current(7, 7).unwrap().thread, "t");
        assert!(!s.current(7, 7).unwrap().loaded);
        assert!(s.current(7, 7).unwrap().turn.is_none());
        assert!(!db.claim_update(5).unwrap());
        assert_eq!(db.offset().unwrap(), 6);
        let job = db.next_job().unwrap().unwrap();
        assert_eq!(job.message.text, "final");
        db.advance(&job).unwrap();
        assert!(db.next_job().unwrap().is_none());
    }
    #[test]
    fn delivery_resumes_at_chunk_boundary_and_retries_do_not_block_other_chats() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(&dir.path().join("db")).unwrap();
        db.enqueue(Outgoing {
            chat: 1,
            text: "a".repeat(6000),
            ..Default::default()
        })
        .unwrap();
        let job = db.next_job().unwrap().unwrap();
        assert!(!db.advance(&job).unwrap());
        let job = db.next_job().unwrap().unwrap();
        assert_eq!(job.part, 1);
        db.retry(job.id, 60).unwrap();
        db.enqueue(Outgoing {
            chat: 2,
            text: "other".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(db.next_job().unwrap().unwrap().message.chat, 2);
    }
}
