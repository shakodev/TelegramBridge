use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Config {
    pub token: String,
    pub users: HashSet<u64>,
    pub projects: BTreeMap<String, PathBuf>,
    pub codex: PathBuf,
    pub database: PathBuf,
    pub sandbox: String,
    pub rpc_timeout: Duration,
    pub approval_ttl: Duration,
}
impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        let token = std::env::var("TELEGRAM_BOT_TOKEN").context("Set TELEGRAM_BOT_TOKEN")?;
        ensure!(!token.trim().is_empty(), "TELEGRAM_BOT_TOKEN is empty");
        let users = std::env::var("TELEGRAM_ALLOWED_USERS")
            .context("Set TELEGRAM_ALLOWED_USERS")?
            .split(',')
            .map(|s| s.trim().parse::<u64>().context("Invalid Telegram user ID"))
            .collect::<Result<HashSet<_>>>()?;
        ensure!(
            !users.is_empty() && users.iter().all(|id| *id > 0 && *id <= i64::MAX as u64),
            "Invalid authorized user IDs"
        );
        let projects = parse_projects(
            &std::env::var("BRIDGE_PROJECTS")
                .context("Set BRIDGE_PROJECTS to a JSON alias/path map")?,
        )?;
        let sandbox = std::env::var("CODEX_SANDBOX").unwrap_or_else(|_| "read-only".into());
        ensure!(
            matches!(sandbox.as_str(), "read-only" | "workspace-write"),
            "CODEX_SANDBOX must be read-only or workspace-write"
        );
        Ok(Self {
            token,
            users,
            projects,
            sandbox,
            codex: std::env::var_os("CODEX_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| "codex".into()),
            database: std::env::var_os("BRIDGE_DATABASE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "data/bridge.sqlite".into()),
            rpc_timeout: Duration::from_secs(30),
            approval_ttl: Duration::from_secs(300),
        })
    }
    pub fn authorized(&self, user: u64, chat: i64, private: bool) -> bool {
        private && chat > 0 && u64::try_from(chat).ok() == Some(user) && self.users.contains(&user)
    }
    pub fn project(&self, alias: &str) -> Result<&Path> {
        let path = self
            .projects
            .get(alias)
            .context("Unknown project alias; use /projects")?;
        ensure!(
            path.canonicalize()? == *path,
            "Project path changed; restart with reviewed configuration"
        );
        Ok(path)
    }
    pub fn thread_params(&self, path: &Path) -> Value {
        json!({"cwd":path,"sandbox":self.sandbox,"approvalPolicy":"untrusted","approvalsReviewer":"user"})
    }
    pub fn sandbox_policy(&self, path: &Path) -> Value {
        if self.sandbox == "read-only" {
            json!({"type":"readOnly","networkAccess":false})
        } else {
            json!({"type":"workspaceWrite","writableRoots":[path],"networkAccess":false,"excludeTmpdirEnvVar":true,"excludeSlashTmp":true})
        }
    }
    pub fn validate_thread(&self, response: &Value, path: &Path) -> Result<()> {
        let cwd = response["cwd"].as_str().context("Missing effective cwd")?;
        ensure!(
            Path::new(cwd).canonicalize()? == path,
            "Codex returned a different workspace"
        );
        ensure!(
            response["approvalPolicy"] == "untrusted" && response["approvalsReviewer"] == "user",
            "Codex did not retain human approval policy"
        );
        let sandbox = &response["sandbox"];
        ensure!(
            sandbox["type"] == "readOnly"
                || (self.sandbox == "workspace-write" && sandbox["type"] == "workspaceWrite"),
            "Unsafe effective Codex sandbox"
        );
        ensure!(
            sandbox["networkAccess"] != true,
            "Unrestricted network access is not allowed"
        );
        if let Some(roots) = sandbox["writableRoots"].as_array() {
            for root in roots {
                ensure!(
                    Path::new(root.as_str().context("Invalid writable root")?)
                        .canonicalize()?
                        .starts_with(path),
                    "Codex writable root escapes project"
                );
            }
        }
        Ok(())
    }
}
pub fn parse_projects(input: &str) -> Result<BTreeMap<String, PathBuf>> {
    let map: BTreeMap<String, PathBuf> =
        serde_json::from_str(input).context("BRIDGE_PROJECTS must be a JSON object")?;
    ensure!(
        !map.is_empty() && map.len() <= 64,
        "Configure 1 to 64 projects"
    );
    map.into_iter()
        .map(|(alias, path)| {
            ensure!(
                !alias.is_empty()
                    && alias.len() <= 32
                    && alias
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
                "Invalid project alias"
            );
            ensure!(
                path.is_absolute() && path.is_dir(),
                "Project must be an existing absolute directory"
            );
            Ok((alias, path.canonicalize()?))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_requires_owner_in_private_chat() {
        let config = Config {
            token: String::new(),
            users: [42].into(),
            projects: BTreeMap::new(),
            codex: "codex".into(),
            database: "test".into(),
            sandbox: "read-only".into(),
            rpc_timeout: Duration::from_secs(1),
            approval_ttl: Duration::from_secs(1),
        };
        assert!(config.authorized(42, 42, true));
        assert!(!config.authorized(43, 43, true));
        assert!(!config.authorized(42, -42, false));
        assert!(!config.authorized(42, 43, true));
    }
    #[test]
    fn aliases_cannot_be_paths_and_roots_must_exist() {
        assert!(parse_projects(r#"{"../bad":"C:/"}"#).is_err());
        assert!(parse_projects(r#"{"ok":"relative"}"#).is_err());
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            parse_projects(&json!({"ok":d.path()}).to_string()).unwrap()["ok"],
            d.path().canonicalize().unwrap()
        );
    }
}
