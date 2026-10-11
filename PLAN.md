# Telegram ↔ Codex bridge implementation plan

## Requirements and architecture

Build a Windows-native Rust application with one persistent Codex App Server child,
newline-delimited bidirectional JSON-RPC, a bounded event loop, teloxide Telegram
long polling, and SQLite durable state. Private chats only; authorize numeric user
IDs before processing any command or callback. Sessions belong to one user/chat
and one configured, canonical project directory. Never import arbitrary Codex
threads or accept arbitrary paths from Telegram.

Codex owns conversation history and sandbox enforcement. The bridge owns routing,
selected sessions, one-time expiring approval tokens, Telegram delivery, and crash
recovery. Explicitly human-reviewed approvals only; no automatic grants, policy
amendments, or session-wide grants. Unknown server requests fail closed. Do not
replay prompts after an ambiguous failure. Resume persisted threads lazily after
reconnection. Bounded queues and coalesced progress keep resource use predictable.

Protocol source: official https://learn.chatgpt.com/docs/app-server and JSON Schema
exported from installed `codex-cli 0.145.0` on 2026-10-11. Prefer generated small
wire enums/contracts over an enormous full-protocol generated dependency.

## Milestones (in dependency order)

- [x] 1. App Server transport. Schema snapshot/code generation, initialize handshake,
  typed envelopes, request correlation, bounded frames/queues, timeout and EOF
  handling. Acceptance: compile; transport tests; real installed server handshake.
- [x] 2. Telegram and sessions (requires 1). Environment config, private-chat ACL,
  project aliases, new/list/switch/resume/cancel commands, isolated prompt routing.
  Acceptance: compile; command, Unicode splitting, authorization and routing tests.
- [ ] 3. Approvals and progress (requires 2). Command/file/permission details,
  Approve/Deny callbacks bound to user/chat/message/turn/server generation, expiry,
  incremental output and final status. Acceptance: compile; denial, replay,
  cross-user, expired and stale-turn tests; no implicit approval path.
- [ ] 4. Persistence and recovery (requires 3). SQLite session/selection/update
  dedup/outbox state, restart supervision, safe interrupted-state recovery, retry
  and rate limits, bounded storage and graceful shutdown. Acceptance: compile;
  reopen/recovery, duplicate update, process failure and delivery retry tests.
- [ ] 5. Full validation and delivery (requires 4). Deterministic integration harness,
  live local App Server smoke test, Windows README and .env.example, format/clippy/
  test/release build. Acceptance: automated suite passes; exact live validation
  scope and any credential-dependent limitations are documented.

## Working agreement

Compile and test after each milestone; fix failures before advancing. Make small
commits for completed, reviewable increments (user requested). Update this file in
each milestone commit with evidence and remaining work. Do not commit credentials.

## Progress

- Initial workspace contains only .gitignore; no existing implementation or graph.
- Rust 1.97.1 and Codex 0.145.0 are installed. Cargo is outside the shell PATH.
- Exported installed App Server JSON Schema; implementation has not started.
- Milestone 1: `cargo test` passes 4 tests (multiplexing, server requests, malformed
  input/EOF, timeout, generated contract). `cargo run --locked` completed a real
  Codex 0.145.0 handshake using an isolated target/smoke-home, without inference.
  Native process execution needed sandbox escalation in this development host.
- Milestone 2: `cargo test --locked` passes 9 tests. Implemented private-chat/user
  ACL, canonical project aliases, new/list/switch/resume/cancel/status commands,
  owner-bound sessions and Unicode-safe plain-text Telegram delivery. Effective
  sandbox and human reviewer settings are checked before any prompt. Approval
  methods currently fail closed; interactive handling is the next milestone.
