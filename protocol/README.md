# Wire contract

Verified against `codex-cli 0.145.0` and the official
[App Server documentation](https://learn.chatgpt.com/docs/app-server) on 2026-10-11.

Run `pwsh -File scripts/export-protocol.ps1 -Codex <path-to-codex.exe>` to refresh
`contract.json` from the installed executable. Review changes before upgrading.
`build.rs` generates the server-request method enum from the exported schema.
The snapshot also retains complete schemas for the security-sensitive approval
requests/responses. Other messages use forward-compatible Serde JSON envelopes.

The transport is one UTF-8 JSON object per line over stdio, without LSP headers.
Codex omits the JSON-RPC `jsonrpc` marker. Initialize once, await its response,
then send `initialized`. IDs can be integers or strings; keep them distinct.
Thread IDs, turn IDs, item IDs, and request IDs identify different objects.

Command/file approvals return `{ "decision": "accept" | "decline" }`.
Permission approvals return `{ "permissions": { ... }, "scope": "turn" }`;
denial uses an empty permission object. The bridge never generates session-wide
grants or policy amendments. Unknown methods return a JSON-RPC error.

`item/agentMessage/delta`, `item/started`, `item/completed`, `turn/started`,
`turn/completed`, `error`, and `serverRequest/resolved` drive the UI. A completed
notification can contain a failed/interrupted turn; inspect its status.
