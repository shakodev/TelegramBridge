use crate::messaging::preview;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub struct Progress {
    pub items: HashMap<(String, String), Value>,
    pub live: HashMap<u64, String>,
}
impl Progress {
    pub fn delta(&mut self, session: u64, text: &str) {
        let entry = self.live.entry(session).or_default();
        entry.push_str(text);
        if entry.len() > 12_000 {
            let mut start = entry.len() - 8000;
            while !entry.is_char_boundary(start) {
                start += 1;
            }
            entry.drain(..start);
        }
    }
    pub fn item(&mut self, thread: &str, item: Value) {
        if self.items.len() >= 128 {
            return;
        }
        if item.to_string().len() > 32_000 {
            return;
        }
        if let Some(id) = item["id"].as_str() {
            self.items.insert((thread.into(), id.into()), item);
        }
    }
    pub fn drain(&mut self) -> Vec<(u64, String)> {
        self.live
            .drain()
            .map(|(id, text)| (id, format!("#{id} · live\n{}", preview(&text, 1600))))
            .collect()
    }
    pub fn clear(&mut self, thread: &str, session: u64) {
        self.items.retain(|(t, _), _| t != thread);
        self.live.remove(&session);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_bounded_and_session_isolated() {
        let mut p = Progress::default();
        p.delta(1, &"🙂".repeat(10000));
        p.delta(2, "other");
        assert!(p.live[&1].len() <= 8000);
        assert_eq!(p.live[&2], "other");
        assert_eq!(p.drain().len(), 2);
        assert!(p.live.is_empty());
    }
}
