# TelegramBridge

[![CI](https://github.com/shakodev/TelegramBridge/actions/workflows/ci.yml/badge.svg)](https://github.com/shakodev/TelegramBridge/actions/workflows/ci.yml)
![Rust 1.89+](https://img.shields.io/badge/Rust-1.89%2B-orange)
![Platform: Windows](https://img.shields.io/badge/platform-Windows-blue)

Run Codex on your Windows machine and talk to it from Telegram. Send prompts, keep
separate conversations for different projects, follow progress, and approve actions
from a private chat with your bot.

The bridge connects to a local Codex App Server over stdio and uses Telegram long
polling. Your machine needs an outbound connection; there is no public server,
webhook, port forwarding, or incoming firewall rule to configure.

[Getting started](#getting-started) · [Configuration](#configuration) ·
[Commands](#telegram-commands) · [Security](#security-and-permissions) ·
[Troubleshooting](#troubleshooting) · [Development](#development)

## Features

- **Multiple sessions.** Work on different projects concurrently and switch
  conversations without interrupting a running task.
- **Progress in chat.** See live excerpts while Codex works, followed by a separate
  final response. Replies carry a session ID so you can tell which task they belong to.
- **Approval buttons.** Review commands, file changes, and permission requests
  with **Approve / Deny** buttons in Telegram.
- **Saved conversations.** Session selections persist in SQLite, with conversation
  history managed by Codex. Resume after restarting the bridge.
- **Controlled access.** Only explicitly allowed Telegram users can interact with
  the bot, and only in their own private chats. Projects are selected by configured aliases.

Written in Rust with Tokio, teloxide, and bundled SQLite. Windows is the deployment
target; CI also builds and tests on Linux.

## Getting started

### Prerequisites

- **Rust 1.89 or newer**, using the MSVC toolchain on Windows.
- **Visual Studio Build Tools**, with the **Desktop development with C++** workload
  and Windows SDK.
- **An installed, authenticated Codex CLI.** The bridge has been tested with
  **Codex 0.145.0**. You need the native `codex.exe`, rather than an npm `.cmd` or
  PowerShell shim. See [protocol compatibility](protocol/README.md) before upgrading.
- **A Telegram bot token** from [BotFather](https://t.me/BotFather) and your numeric
  Telegram user ID. The ID is a number, not your `@username`.

SQLite is compiled into the executable; no database server is needed. Use a
dedicated bot with no webhook and no other polling client.

### 1. Clone and configure

In PowerShell:

```powershell
git clone https://github.com/shakodev/TelegramBridge.git
cd TelegramBridge
Copy-Item .env.example .env
notepad .env
```

Fill in the bot token, allowed user IDs, project directories, and Codex executable
path. For example:

```dotenv
TELEGRAM_BOT_TOKEN=123456789:replace_with_BotFather_token
TELEGRAM_ALLOWED_USERS=123456789
BRIDGE_PROJECTS='{"demo":"C:/Projects/Demo","website":"C:/Projects/Website"}'
CODEX_BIN='C:/Tools/Codex/codex.exe'
CODEX_SANDBOX=read-only
BRIDGE_DATABASE=data/bridge.sqlite
```

Replace the example paths with existing directories and the location of your native
executable. Use forward slashes in the JSON project map to avoid escaping Windows
backslashes. Keep `.env` in the bridge directory, outside the projects Codex can access.

The bridge uses the authentication and configuration of the Codex process it
starts. Make sure that executable works with your chosen provider before continuing.
For a separate configuration and history directory, see [`CODEX_HOME`](#configuration).

### 2. Build and run

```powershell
cargo build --locked --release
.\target\release\telegram-codex-bridge.exe
```

Keep this terminal open and the machine awake. Run the bridge as your normal
Windows user. Press **Ctrl+C** to stop polling and close the Codex process tree.

### 3. Start a conversation

Open a private chat with your bot and send these messages one at a time:

```text
/start
/projects
/new demo
Explain how this project is organized and where its entry point is.
```

`/new demo` creates and selects a session. After that, ordinary text becomes a
prompt in that session. Use `/new website` for another project and `/sessions` to
see your conversations.

The default sandbox is `read-only`. To let Codex edit project files, set
`CODEX_SANDBOX=workspace-write` in `.env` and restart the bridge.

## Configuration

The bridge loads `.env` from its **current working directory**. Existing process
environment variables take precedence. Restart after changing configuration.

| Variable | Required | Default / purpose |
| --- | --- | --- |
| `TELEGRAM_BOT_TOKEN` | Yes | Token issued by BotFather. |
| `TELEGRAM_ALLOWED_USERS` | Yes | Comma-separated numeric Telegram user IDs. |
| `BRIDGE_PROJECTS` | Yes | JSON map of aliases to existing absolute directories. Supports 1–64 projects. |
| `CODEX_BIN` | No | `codex` on `PATH`. An explicit path to the native `codex.exe` is recommended on Windows. |
| `CODEX_SANDBOX` | No | `read-only`. Also accepts `workspace-write`. |
| `BRIDGE_DATABASE` | No | `data/bridge.sqlite`. Relative paths resolve from the working directory. |
| `CODEX_HOME` | No | Inherited by Codex. Set it to use a separate Codex configuration and history directory. |
| `RUST_LOG` | No | `telegram_codex_bridge=info`. Controls bridge logging. |

Project aliases may contain letters, numbers, underscores, and hyphens, up to 32
characters. Telegram commands accept these aliases; they cannot supply arbitrary
filesystem paths.

If you use a separate Codex home, authenticate and configure it before starting
the bridge. Set the same `CODEX_HOME` in the PowerShell session used for Codex login
and in the bridge environment. API-key providers can use `OPENAI_API_KEY` through
their Codex configuration. The bridge does not read or copy Codex authentication tokens.

## Telegram commands

| Command | Action |
| --- | --- |
| `/start` or `/help` | Show available commands. |
| `/projects` | List configured project aliases. |
| `/project demo` | Set the project used by subsequent `/new` commands. |
| `/new [demo]` | Create and select a session, optionally choosing a project. |
| `/sessions` or `/list` | List sessions owned by your user and chat. |
| `/switch 2` or `/resume 2` | Select a saved session and load its Codex context as needed. |
| `/status` | Show the selected session's state. |
| `/cancel [2]` | Interrupt the selected session, or the specified session. |
| Plain text | Send a prompt to the selected session. |

Session numbers are bridge IDs, not Codex thread IDs. Each session can run one
turn at a time; different sessions can run concurrently. Switching sessions does
not cancel work. Replies include `#ID` even if you have since selected another session.

Live excerpts update about every three seconds. Final responses arrive separately
and are split to fit Telegram's message limits. Text is sent literally, without
Markdown parsing. Only text prompts are supported.

## Security and permissions

Only allowlisted users in their own private chats can use the bridge. All allowed
users share the configured projects and the host's Codex account: use this with
people you trust. Session ownership separates conversations, not operating-system access.

The bridge requests Codex's `untrusted` approval policy with a human reviewer,
checks the effective settings, and applies a restricted sandbox policy on each
turn. Network access is disabled in that policy. `workspace-write` permits writes
within the selected project; it does not mean every edit requires an approval.
Codex is responsible for enforcing the sandbox.

When Codex requests approval, the bot shows the command, working directory, reason,
file changes, or requested permissions. Buttons authorize that action, or that
turn for permission grants. They expire after **five minutes** and are tied to the
requesting user, chat, message, session, turn, and current server connection.
Reconnecting invalidates outstanding buttons. Unsupported, incomplete, and oversized
requests are denied. Interactive tool questions, MCP elicitation, external tools,
and legacy approvals are currently unsupported and fail closed.

Project directories are canonicalized and rechecked on resume. File-change and
permission approvals reject paths outside the workspace, symlink escapes, globs,
and special roots. Standard Codex read access can extend beyond the working
directory, so project selection is not a confidentiality boundary. An approved
unsandboxed command can affect the host as described in its approval request.
Use a dedicated, restricted Windows account if you need stronger host isolation.

Keep bot tokens in environment variables or the ignored `.env` file. The bridge
removes Telegram bot credentials from the Codex child's environment, but `.env`
must still stay outside agent-accessible projects. Protect the bridge directory,
SQLite files, and Codex home with Windows user permissions. Local history is
plaintext, and Telegram bot chats are not end-to-end encrypted.

## Running unattended and recovery

For unattended use, create a Windows Task Scheduler task with the release
executable, your normal user account, a fixed **Start in** directory, and
restart-on-failure. Run only one bridge instance per bot and database.

Back up both the SQLite database and the Codex home: the database stores bridge
sessions and selections; Codex stores conversation history. Sessions without a
first turn are recreated under the same bridge ID after a restart.

If Codex exits, the bridge reconnects with backoff and resumes threads when needed.
RPC calls time out after 30 seconds, and turns are interrupted after one hour.
On Windows, Job Objects terminate child processes when the bridge exits.

Prompts are claimed durably before execution and **never automatically replayed**
after a crash. If a session reports an unknown outcome, inspect `/status` and
`/resume ID` before sending another prompt. Final replies are queued in SQLite
and retried, including Telegram rate-limit delays. A crash after Telegram accepts
a message but before its checkpoint can produce a duplicate chunk. Permanent
delivery failures are logged; the last 64 are retained in the `failed_delivery`
table. Removed users receive no queued output.

<details>
<summary>Resource limits</summary>

The bridge caps sessions at 256, pending approvals at 128, RPC/event queues at 128
slots, and JSON frames at 2 MiB. The durable outbox holds up to 1,024 jobs within an
approximately 8 MiB queued-JSON budget. SQLite uses a 2 MiB cache, and the runtime
uses two Tokio workers.

Final output over 128,000 bytes is visibly truncated; full context remains in Codex
history. Approval details over 12,000 UTF-16 units are denied rather than partially
shown. Queue and frame overflows fail closed. Progress and stale approvals are disposable.

</details>

## Troubleshooting

| Symptom | Check |
| --- | --- |
| `cargo` is not recognized | Reopen PowerShell after installing Rust, or add `$env:USERPROFILE\.cargo\bin` to `PATH`. |
| Codex is unavailable or keeps reconnecting | Check `CODEX_BIN`, use a native `.exe`, and verify its authentication and configuration. Run the handshake check below. |
| The bot does not reply | Check the token, numeric user allowlist, and that you are messaging from your own private chat. Ensure the bot has no webhook or second polling client. |
| A project cannot be selected or resumed | Confirm its configured directory exists and has not moved. Restart after reviewing path changes. |
| Codex cannot edit files | The default is `read-only`. Select `workspace-write` and restart if you want project edits. |
| An approval button no longer works | Buttons expire after five minutes and are invalidated when Codex reconnects. Inspect the session before retrying the task. |

To check the App Server handshake without starting Telegram polling:

```powershell
$env:CODEX_BIN = 'C:/Tools/Codex/codex.exe'
.\target\release\telegram-codex-bridge.exe --check-codex
```

This check reads `CODEX_BIN` from the shell; it does **not** load `.env`. A successful
check prints `Codex App Server handshake successful`. It verifies the connection,
not provider authentication or a complete model turn.

## Development

Run the same checks as CI:

```powershell
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo build --locked --release
```

The default suite uses a mock Codex process and a local Telegram HTTP fixture.
It covers session isolation, approvals, cancellation, recovery, Telegram retries,
and button delivery without bot credentials or hosted inference.

An optional test exercises a real installed Codex App Server with an isolated
temporary home and a local model fixture:

```powershell
$env:CODEX_BIN = 'C:/Tools/Codex/codex.exe'
cargo test --locked --test live_codex -- --ignored --nocapture
```

It checks initialization, a streamed turn, restart, resume, and thread listing.
Real Telegram account delivery requires your own bot credentials and is outside
the credential-free suite.

The checked-in [wire contract](protocol/README.md) comes from the installed CLI's
JSON Schema. `build.rs` generates server-method bindings from
[`protocol/contract.json`](protocol/contract.json). To refresh it before a reviewed
Codex upgrade:

```powershell
pwsh -File scripts/export-protocol.ps1 -Codex 'C:/Tools/Codex/codex.exe'
```

For protocol details, see the [Codex App Server documentation](https://learn.chatgpt.com/docs/app-server),
[Telegram Bot API](https://core.telegram.org/bots/api), and
[Telegram rate-limit guidance](https://core.telegram.org/bots/faq#my-bot-is-hitting-limits-how-do-i-avoid-this).

Issues and pull requests are welcome. For bug reports, include the bridge commit,
Codex version, relevant configuration with secrets removed, and steps to reproduce.
Run the CI checks before submitting code changes.

## License

MIT, as declared in [Cargo.toml](Cargo.toml).
