# Telegram ↔ Codex bridge

Windows-native Rust bridge using Tokio, teloxide, Serde and bundled SQLite. One
Codex App Server process serves independent Telegram sessions over bidirectional
stdio JSON-RPC. No HTTP listener or incoming firewall rule is needed.

## Windows setup

1. Install stable Rust **1.89+** with the MSVC toolchain and Visual Studio Build
   Tools **Desktop development with C++** (including the Windows SDK). SQLite is
   built into the executable; no separate database installation is needed.
2. Install Codex and locate the native `codex.exe`. This implementation was tested
   with **Codex 0.145.0**. An npm `.cmd` shim is not a native executable.
3. Create a bot through [BotFather](https://t.me/BotFather). Obtain your own numeric
   Telegram user ID from a trusted source. Use a dedicated bot, with no webhook
   and no other polling client. Send `/start` to it from your private chat.
4. Copy `.env.example` to `.env`; fill in the token, allowed numeric user IDs, native
   Codex executable and project aliases. Multiple user IDs are comma-separated.
   Put credentials only in environment variables or the ignored `.env` file.
5. Authenticate Codex using your configured provider. If using a separate
   `CODEX_HOME`, set it in PowerShell before running `codex login`. The bridge
   inherits Codex authentication/configuration; it never reads or copies tokens.
   API-key providers may use `OPENAI_API_KEY` through their Codex configuration.

From the repository in PowerShell:

```powershell
Copy-Item .env.example .env  # edit before launching
cargo test --locked --all-features
cargo build --locked --release
.\target\release\telegram-codex-bridge.exe
```

If Cargo is not on PATH: `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"`.
Run as your normal Windows user. Keep the working directory consistent so `.env`
and the relative database path resolve consistently. For unattended operation,
use Windows Task Scheduler with this executable, a fixed **Start in** directory,
your normal user account, and restart-on-failure. Allow only one instance per bot
and database. Ctrl+C stops polling and closes the Codex process tree.

## Telegram commands

| Command | Action |
| --- | --- |
| `/help` or `/start` | Show help |
| `/projects` | List configured project aliases |
| `/project demo` | Select the workspace for new sessions |
| `/new [demo]` | Create and select a session |
| `/sessions` or `/list` | List only sessions owned by this user/chat |
| `/switch 2` or `/resume 2` | Select a saved session; load Codex context as needed |
| `/status` | Show the selected session's state |
| `/cancel [2]` | Interrupt a running turn in the selected or named session |
| Plain text | Start a turn in the selected session |

Session IDs shown in Telegram are bridge IDs, not arbitrary Codex thread IDs.
Switching never cancels another session. One turn may run per session; multiple
sessions can run concurrently. Replies include `#ID` regardless of current
selection. Live excerpts update about every three seconds; final responses are
separate messages. Text is sent literally, split safely for Telegram's limits.

Approval messages display the command, cwd, reason, file changes/diff or requested
permissions, followed by **Approve / Deny**. Buttons authorize one action, or one
turn for permission grants. They expire after five minutes and bind the requesting
user, private chat, message, session, turn and current server lifetime. Missing,
oversized or unsupported requests are denied. MCP elicitation, interactive tool
questions, external tools and legacy approvals currently fail closed.

## Security and operational behavior

- Only allowlisted numeric users in their own private chats can act. Group,
  channel, inline and bot-originated requests are ignored. All authorized users
  share the configured project allowlist and the host's Codex account; they should
  be mutually trusted operators. Session ownership prevents routing mixups, not
  operating-system isolation between mutually hostile users.
- Paths come from canonical, existing configured directories, never Telegram text.
  Resume revalidates the original workspace. File-change/permission approvals
  reject paths outside that workspace, symlink escapes, globs and special roots.
- Default sandbox is `read-only`; opt into `workspace-write` in configuration.
  The bridge requests `untrusted` approval policy and a human reviewer, verifies
  the effective settings, and applies a restricted network/write policy each
  turn. It never selects full access, auto-review, persistent policy amendments
  or session-wide approval. Codex remains responsible for sandbox enforcement.
  Standard Codex read access is broader than cwd; project selection is not a
  confidentiality boundary. An explicitly approved unsandboxed command has the
  effects described in its approval request. Use a dedicated restricted Windows
  account and reviewed Codex configuration when stronger host isolation is needed.
- Telegram bot credentials are removed from the Codex child's environment. Do not
  place the bridge's `.env` inside an agent-accessible project. Protect the bridge
  directory, SQLite files and Codex home with your Windows user ACLs. SQLite and
  Codex history are local plaintext; Telegram bot chats are not end-to-end encrypted.
- Sessions/selections persist in SQLite; Codex keeps conversation history in its
  home. Preserve both. Empty sessions are recreated after restart under the same
  Telegram ID because Codex does not persist a rollout before the first turn.
- RPC timeout is 30 seconds. Turns are interrupted after one hour; a failed
  interruption triggers process recovery. Codex restarts with bounded backoff,
  and threads resume lazily. Windows Job Objects kill descendants on bridge exit.
- Updates are claimed durably **before** side effects and acknowledged to Telegram
  after processing. A crash in that window can leave an unsubmitted or partially
  executed prompt; it is never automatically replayed. Inspect the session and
  send a new prompt explicitly. Approval buttons are invalidated on reconnect.
- Final output uses a durable outbox with exponential retries and Telegram
  `retry_after` handling. Completed chunks are checkpointed. A crash after Telegram
  accepts a send but before its checkpoint can duplicate that chunk. Permanent
  delivery failures are logged; the last 64 durable messages are retained in the
  SQLite `failed_delivery` table for local inspection. Removed users receive no
  queued output. Progress and stale approvals are disposable.
- Resource bounds: 256 sessions, 128 pending approvals, 128 RPC/event slots,
  2 MiB per JSON frame, 1,024 outbox jobs / approximately 8 MiB of queued JSON,
  2 MiB SQLite cache, two Tokio workers. Outputs over 128 KiB are visibly truncated;
  approval details over 12,000 UTF-16 units are denied, never partially approved.
  Queue/frame overflow fails closed. Full context remains in Codex history.

## Verification and protocol updates

```powershell
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
# Installed-server check: isolated temporary home and loopback model fixture.
# No user credentials, hosted inference, or Telegram traffic:
cargo test --locked --test live_codex -- --ignored --nocapture
# Handshake only (uses CODEX_BIN from the shell, not .env):
.\target\release\telegram-codex-bridge.exe --check-codex
# Refresh the small checked-in contract before a reviewed Codex upgrade:
pwsh -File scripts/export-protocol.ps1 -Codex 'C:/path/to/codex.exe'
```

Tests exercise the actual teloxide HTTP client against a local Telegram fixture,
including 429 retry and approval-button delivery, and a separate fake Codex
process for session isolation, all approval types, cancellation and recovery.
The opt-in live test drives the real installed App Server through initialization,
thread creation, streaming a completed turn, process restart, resume and list,
with a local Responses SSE fixture. Live Telegram account delivery requires your
bot credentials and is not part of the credential-free suite.

Protocol authority: [official App Server documentation](https://learn.chatgpt.com/docs/app-server)
and the installed CLI's JSON Schema. `build.rs` generates server-method bindings
from `protocol/contract.json`; approval schemas are retained there for review.
Telegram behavior follows the [Bot API](https://core.telegram.org/bots/api) and
[rate-limit guidance](https://core.telegram.org/bots/faq#my-bot-is-hitting-limits-how-do-i-avoid-this).
See `PLAN.md` for milestones and validation evidence.
