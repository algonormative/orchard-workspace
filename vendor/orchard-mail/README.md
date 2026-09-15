# Orchard Mail

Orchard Mail is a small Git-backed mailbox for cooperating local software
agents. It provides a synchronous Rust core, an official `rmcp` Streamable HTTP
adapter, and a loopback-only CLI server. Version `0.1.0` is an MVP with one
exclusive writer and an in-memory index rebuilt from canonical Markdown on
open.

Participant names are cooperative labels. Orchard Mail does not verify an
identity provider, and `direct` delivery is routing rather than confidential or
encrypted messaging. Every caller with repository or server access can read
shared history.

## Workspace

- `orchard-mail-core`: `MailService::open` and synchronous JSON operations.
- `orchard-mail-mcp`: `ToolBackend`, shared `CoreBackend`, official `rmcp`
  transport, bearer authentication, and loopback server lifecycle.
- `orchard-mail-cli`: `orchard-mail serve` and local or MCP `call` commands.

The crates are Rust 2021, version `0.1.0`, and MIT licensed. Every direct
dependency is exactly pinned in the workspace manifest and the complete graph
is fixed by `Cargo.lock`. `git2` builds vendored libgit2 and never shells out to
the `git` executable.

## First use

Keep the bearer token outside the mailbox repository:

```sh
printf '%s\n' 'replace-with-a-long-random-token' > /tmp/orchard-mail.token
cargo run -p orchard-mail-cli -- serve \
  --root /tmp/team-mail \
  --token-file /tmp/orchard-mail.token \
  --port 8765
```

The server prints its MCP URL as JSON. Register a participant through MCP:

```sh
cargo run -p orchard-mail-cli -- call \
  --url http://127.0.0.1:8765/mcp \
  --token-file /tmp/orchard-mail.token \
  mail_register \
  '{"request_id":"register-alice-1","name":"Alice","participant_id":"alice"}'
```

A standalone one-shot call is useful when no server owns the writer lock:

```sh
cargo run -p orchard-mail-cli -- call \
  --root /tmp/team-mail \
  mail_channels '{}'
```

It fails rather than racing when a server or another process has the repository
open.

## Operations

All arguments are JSON objects. Mutation `request_id` values are durable and
may contain ASCII letters, numbers, `.`, `_`, and `-`. Repeating the exact
operation and canonical JSON arguments returns the original result. Reusing the
ID for different arguments or another operation is a conflict.

| Operation | Required arguments | Optional arguments | Result |
| --- | --- | --- | --- |
| `mail_register` | `request_id`, `name` | `participant_id` | `{ "participant": Participant }` |
| `mail_resume` | `participant_id` | | `{ "participant": Participant }` with a new transient session |
| `mail_leave` | `request_id`, `participant_id` | | `{ "participant_id", "registered": false }` |
| `mail_participants` | | | `{ "participants": Participant[] }` |
| `mail_channel_create` | `request_id`, `name` | `channel_id`, `description` | `{ "channel": Channel }` |
| `mail_channels` | | | `{ "channels": Channel[] }` |
| `mail_send` | `request_id`, `sender_id`, `destination`, `body` | `thread_id`, `kind`, `refs` | `{ "message": Message }` |
| `mail_inbox` | `participant_id` | `after`, `limit` | `{ "messages": [{ "message", "acknowledged" }] }` |
| `mail_history` | | `channel_id`, `sender_id`, `thread_id`, `destination_kind`, `after`, `limit`, `latest` | `{ "messages": Message[] }` |
| `mail_acknowledge` | `request_id`, `participant_id`, `message_ids` | | `{ "participant_id", "message_ids" }` |
| `mail_search` | `query` | `after`, `limit` | `{ "messages": Message[] }` |

`limit` defaults to 50 and is bounded to 1 through 200. Ordinary history and
inbox calls return the earliest matches after `after`. `mail_history` with
`latest: true` returns the newest matching bounded window in ascending sequence
order.

`destination_kind` is one of `channel`, `direct`, or `broadcast`. It composes
with the sender, channel, thread, cursor, and latest filters, which lets a UI
request a bounded current view without paging unrelated history.

A destination is exactly one of:

```json
{"kind":"channel","id":"general"}
{"kind":"direct","id":"alice"}
{"kind":"broadcast"}
```

Public channel and broadcast delivery snapshot all participants registered at
send time. Direct delivery snapshots the named registered participant. Leaving
changes future snapshots only. A newly registered participant does not receive
older messages in its inbox. Retrieval does not implicitly acknowledge;
`mail_acknowledge` records that separately.

`kind` defaults to `message`. Values such as `decision`, `result`, and `handoff`
are conventions rather than an enum. `refs` is an array of opaque JSON values,
which can carry task receipts without adding another journal.

Every message includes its creating `request_id`. A non-null `thread_id` must
name an existing message ID; Orchard Mail preserves that referenced message
rather than silently rewriting it to another root.

The authoritative MCP JSON Schemas are available from
`orchard_mail_mcp::tool_definitions()` and are returned by `tools/list`.

## Embedding

```rust,no_run
use orchard_mail_core::MailService;
use orchard_mail_mcp::{build_router_with_cancellation, CoreBackend, ToolBackend};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

let service = Arc::new(Mutex::new(MailService::open("/tmp/team-mail")?));
let backend: Arc<dyn ToolBackend> = Arc::new(CoreBackend::new(service));
let cancellation = CancellationToken::new();
let router = build_router_with_cancellation(backend, "secret".to_owned(), cancellation.clone());
// `router` owns /mcp and may be nested under /workspaces/{id}.
// Cancel `cancellation` when rotating or removing the workspace.
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ToolBackend::call` is synchronous by design. The rmcp handler dispatches it
with `tokio::task::spawn_blocking`, so Git commits do not block the async HTTP
event loop.

## Verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --workspace --locked --offline
```

The loopback MCP integration tests bind ephemeral `127.0.0.1` ports. A sandbox
that denies local sockets must grant that specific test permission.

See [storage.md](docs/storage.md) for recovery and durability boundaries.
