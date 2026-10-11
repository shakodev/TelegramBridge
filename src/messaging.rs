use anyhow::{Result, bail, ensure};

pub const HELP: &str = "Commands:\n/projects — allowed workspaces\n/project ALIAS — select workspace for new sessions\n/new [ALIAS] — create a session\n/sessions — list your sessions\n/switch ID — select a session\n/resume ID — resume and select a saved session\n/cancel [ID] — interrupt a turn\n/status — current session\nSend plain text as a prompt.";
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Projects,
    Project(String),
    New(Option<String>),
    Sessions,
    Switch(u64),
    Resume(u64),
    Cancel(Option<u64>),
    Status,
    Prompt(String),
}
pub fn parse(text: &str) -> Result<Command> {
    let text = text.trim();
    ensure!(
        !text.is_empty() && text.len() <= 32_000,
        "Prompt must contain 1–32000 bytes"
    );
    if !text.starts_with('/') {
        return Ok(Command::Prompt(text.into()));
    }
    let mut parts = text.split_whitespace();
    let name = parts.next().unwrap();
    let arg = parts.next();
    ensure!(parts.next().is_none(), "Too many command arguments");
    let required = || arg.ok_or_else(|| anyhow::anyhow!("Missing command argument"));
    Ok(match (name, arg) {
        ("/help" | "/start", None) => Command::Help,
        ("/projects", None) => Command::Projects,
        ("/project", _) => Command::Project(required()?.into()),
        ("/new", _) => Command::New(arg.map(str::to_owned)),
        ("/sessions" | "/list", None) => Command::Sessions,
        ("/switch", _) => Command::Switch(required()?.parse()?),
        ("/resume", _) => Command::Resume(required()?.parse()?),
        ("/cancel", _) => Command::Cancel(arg.map(str::parse).transpose()?),
        ("/status", None) => Command::Status,
        _ => bail!("Unknown command. Use /help"),
    })
}
/// Telegram limits are UTF-16 based. No markup parser means untrusted text is literal.
pub fn split_text(text: &str, limit: usize) -> Vec<String> {
    assert!(limit >= 2);
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > limit {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push(c);
        units += c.len_utf16();
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}
pub fn preview(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.into()
    } else {
        format!(
            "{}\n[truncated]",
            text.chars().take(limit).collect::<String>()
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_are_strict() {
        assert_eq!(parse("/cancel 3").unwrap(), Command::Cancel(Some(3)));
        assert_eq!(
            parse("hello world").unwrap(),
            Command::Prompt("hello world".into())
        );
        assert!(parse("/new a b").is_err());
        assert!(parse("/switch ../x").is_err());
    }
    #[test]
    fn unicode_splitting_is_lossless() {
        let input = "🙂a<&>\n".repeat(2500);
        let chunks = split_text(&input, 3800);
        assert_eq!(chunks.concat(), input);
        assert!(chunks.iter().all(|c| c.encode_utf16().count() <= 3800));
    }
}
