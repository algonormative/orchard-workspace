# Orchard workspace host

`orchard-workspace-host` is the reusable process core for the Orchard web
server and headless tests. One `WorkspaceHost` owns the app data root, every
open mail repository, every attached task-store command lane, the browser API,
and all workspace MCP endpoints. Orchard does not start a child mail daemon.

## Process and storage ownership

`WorkspaceHost::open(data_root, br_path)` takes an exclusive advisory lock on
`data_root/host.lock` for the host lifetime. A second process cannot load stale
configuration or race port assignment. `config.json` and credentials are
written atomically. The data root and `credentials/` are owner-only; token
files are mode `0600` on Unix.

`WorkspaceHost::open_with_port(data_root, br_path, Some(port))` deliberately
replaces a different persisted port before binding. The CLI uses this for an
explicit `--port`; passing `None` preserves the normal persisted-port behavior.

The persisted loopback port is allocated once and then reused. Startup binds
only `127.0.0.1`. If that exact saved port is occupied, startup fails instead
of silently changing client configuration.

Each workspace has:

- a root and Git-backed `mail/` repository;
- durable human participant `owner` and system participant `orchard`;
- an app-owned classic Beads store at `tasks/.beads/` when the reviewed `br`
  binary is available;
- optional Git project references and attached task stores, stored as
  canonical-path metadata only. Adding a project inspects only that project's
  root `.beads/beads.db` and links a compatible store automatically.

Repository attachment uses `git2`; Orchard never runs an external `git`
command and never clones, checks out, creates a worktree, or commits attached
code. Beads discovery is a read-only schema inspection: Orchard does not run
`br init`, migrate, or modify an unsupported external store. A project without
Beads, including a bare repository, remains attached with `task_status: none`.
An incomplete or unsupported store remains attached with `task_status` and
`task_error` describing why tasks are unavailable. Archiving retains all files
and metadata while immediately removing the workspace endpoint and write
access. Missing repositories and linked stores appear explicitly in views.

## Rust interface

```rust
let host = Arc::new(WorkspaceHost::open(data_root, br_path)?);
let server = host.clone().start_server_with_ui(static_router).await?;
let value = host.call("workspace_list", json!({}))?;
```

`start_server()` starts the same API without a static UI router. `ServerHandle`
reports its socket address and performs cancellation plus a bounded three
second drain on shutdown. `owner_bootstrap()` retains the endpoint and owner
credential path/value for programmatic compatibility. The executable no longer
prints that credential, and the browser does not receive it. It is not exposed
through browser or MCP calls.

Direct host operations are an allowlist:

| Operation | Arguments |
| --- | --- |
| `workspace_list` | `{}` |
| `workspace_create` | `{name, root?, owner_name?}` |
| `workspace_archive` | `{workspace_id}` |
| `workspace_snapshot` | `{workspace_id, history_limit?}` |
| `workspace_info` | `{workspace_id}`; sanitized and also available to that workspace's MCP clients |
| `workspace_intro` | `{workspace_id}`; README, current participants/channels, introduction, and joining prompt |
| `workspace_status` | `{workspace_id}`; counts, participant records, artifact availability, and source errors |
| `workspace_alerts` | `{workspace_id, participant_id, after?, limit?, include_channel_messages?}` |
| `connection_info` | `{workspace_id}`; direct call only, returns that workspace's MCP endpoint and token |
| `rotate_token` | `{workspace_id}` |
| `repository_attach` / `repository_detach` | `{workspace_id, path}` / `{workspace_id, repository_id}`; attach returns `{repository, task_store, attached, task_store_attached}` |
| `task_store_attach` / `task_store_detach` | `{workspace_id, path}` / `{workspace_id, store_id}` |
| `tasks_list` | `{workspace_id, store_id, status?}` |
| `task_show` | `{workspace_id, store_id, task_id}` |
| `task_create` | `{workspace_id, store_id, request_id, title, description?, priority?, labels?}` |
| `task_update` | `{workspace_id, store_id, task_id, request_id, title?, description?, status?, priority?, add_labels?, remove_labels?}` |
| `task_close` | `{workspace_id, store_id, task_id, request_id, reason?}` |
| `task_dependencies` | `{workspace_id, store_id, task_id}`; read-only in this release |
| `resource_get` / `resource_links` | `{workspace_id, ref}` |
| `resource_link` | `{workspace_id, source, target, label?, request_id}` |
| `artifact_roots` | `{workspace_id}` |
| `artifact_list` | `{workspace_id, root_id, path?, revision?}` |
| `artifact_history` | `{workspace_id, root_id, path}` |
| `artifact_upload` | `{workspace_id, path, content_base64, request_id}` |
| `artifact_delete` | `{workspace_id, path, request_id}`; owned artifacts only |
| `artifact_commit` | `{workspace_id, paths, request_id, message?}`; exact local-change set in owned artifacts only |
| `settings_get` | `{}`; visible JSON without credentials |

All eleven `mail_*` operations from Orchard Mail are also accepted directly
with `workspace_id` added to their normal arguments. `workspace_id` is stripped
before the shared `MailService` call. Task responses always carry a qualified
`task_ref: {store_id, task_id}`, because different stores may contain the same
task ID.

Repository views add `name`, `task_store_id`, `task_status`, and `task_error`.
`task_status` is one of `linked`, `none`, `missing`, or `unsupported`.
Task-store views add `name`, `source`, and `repository_id`; `source` is one of
`owned`, `repository`, or `external`. The app-owned `default` store is listed
first when present. Manually attached legacy stores remain `external` and stay
queryable independently.

Repeating `repository_attach` is idempotent and re-runs discovery, so a project
first added without Beads can acquire a later-created compatible store. The
same canonical database may be reused by a project and a manual source in one
workspace, but cannot be owned by two workspaces. `repository_detach` removes
the project association while retaining its task store as an external source.
`task_store_detach` never deletes files, refuses the owned store, and clears any
project links to the detached source.

`workspace_info` also returns authenticated same-host paths for the workspace,
owned artifact root, and its `README.md`, plus the available operation names.
Workspace creation seeds a minimal goals/context/MOTD README in the owned Git
artifact root. Startup repairs only an interrupted matching seed; it never
overwrites a user file or resurrects a README that appeared in repository
history and was later deleted. `workspace_intro` is read-only and dynamically
adds current participants and channels. Its generic joining prompt names the
workspace MCP endpoint and authenticated same-host credential-file path, but
never credential contents. It directs every provider through the same
capability discovery, registration/resume, emergent task choice, message
coordination, alert acknowledgement, and linked-evidence handoff flow. The
invitation authorizes one bounded task or review pass and requires the agent to
stop on completion, blockage, or lack of suitable work. Workspace records
provide details within that scope without expanding harness or provider
permissions. Remote agents use Orchard Settings connection setup because a
local credential path is not transferable.
`workspace_status` reports source errors instead of describing registration
contact as active work.

`workspace_alerts` paginates complete Mail history with a stateless sequence
cursor. It excludes self-authored messages, detects exact mentions outside code,
follows reply chains to their root author, and includes ordinary channel posts
only when requested. Scanning does not acknowledge messages. Artifact root
views expose absolute same-host paths and mark only the validated owned root as
`writable`; attached repositories remain read-only. Direct commit and delete
operations accept only explicit owned-artifact paths, require the entire Git
status to match, and use durable idempotency receipts. See
[resources.md](resources.md) for bounds and recovery details.

`workspace_snapshot` has one stable flattened shape:

```json
{
  "workspace": {"id": "...", "repositories": [], "task_stores": []},
  "mail": {"participants": [], "channels": [], "history": []},
  "repositories": [],
  "task_stores": [{"store": {}, "tasks": []}],
  "errors": []
}
```

Mail history requests use `latest: true`, so a busy workspace returns its
newest bounded activity rather than freezing at the first 200 messages.

## Browser and MCP authentication

The executable supplies an embedded static router to
`start_server_with_ui`. The host owns these same-origin API routes:

- `GET /api/session` returns `{"authenticated": bool}`.
- `POST /api/session` accepts `{}` from the exact loopback Origin and Host and
  sets a random, in-memory, host-only `orchard_session` cookie with `HttpOnly`,
  `SameSite=Strict`, and `Path=/api`. An optional legacy owner token remains
  accepted for programmatic compatibility; an invalid supplied token is
  rejected.
- `DELETE /api/session` clears the session and cookie.
- `POST /api/call` accepts `{"operation": "...", "args": {}}` and returns
  `{"result": ...}` or `{"error": "..."}`.
- `GET /api/workspaces/{workspace_id}/resource?href=...` resolves one
  canonical resource permalink.
- `GET /api/workspaces/{workspace_id}/artifact/download?...` serves a bounded
  authenticated download or a magic-verified safe raster preview.

Every browser POST and DELETE requires the exact Origin and Host for
`http://127.0.0.1:<persisted-port>`; missing, foreign, and opaque origins and
alternate hosts are rejected. Browser GETs require the exact Host and reject a
present mismatched Origin. POST bodies must be JSON and are capped at 1 MiB.
There is no permissive CORS. Browser sessions disappear on restart and the UI
refreshes them once after an explicit 401 without receiving a credential. The
retained owner credential is separate from workspace MCP credentials and is
never put in a URL or frontend bundle.

Resource GET routes accept the owner session/credential or the bearer for the
exact workspace in the path. They reject a foreign `Origin`, scope nested
references to the same workspace, and return `Cache-Control: private,
no-store`. See [resources.md](resources.md) for reference, Git, crosslink,
upload, and download details.

Each active workspace serves Streamable HTTP MCP at
`/workspaces/{workspace_id}/mcp` with its own bearer token. Rotation cancels
existing router sessions before installing the new token; archive and host
shutdown also cancel sessions. MCP exposes the mail tools, task operations, and
sanitized `workspace_info`, orientation/status/alert operations, resource
tools, and artifact tools. It does not expose workspace creation/archive,
attachments, token rotation, `connection_info`, or browser credentials.

## Beads safety and retries

The reviewed helper is `br 0.1.14` from source commit
`beff256b491e20508eab0319b23547ff145cfd04`. The packaged arm64 binary SHA-256
is `8c1a0024e35535e49cd1ee97cd432f06e28bf623afbac7fcc6c19f7d3c249ba9`.
The host accepts only schema version 1 with the exact reviewed table/index SQL
fingerprint. Attachment opens SQLite read-only and completes this check before
any `br` command. Unknown formats are never imported or migrated.

Commands use argument arrays, never a shell, with a 1.5 second SQLite lock
timeout, ten second process deadline, and 8 MiB output cap. Operations on one
canonical database path are serialized even if path aliases or workspace IDs
differ. The same canonical database cannot be attached to two workspaces.
External JSONL changes are explicitly imported only after schema validation.
Mutations check the JSONL watermark again before an explicit `sync
--flush-only`, so an external writer cannot be silently overwritten.

Task mutations require `request_id`. An immutable mail intent stores a
fingerprint of the complete semantic request before `br` runs; a result or
unknown receipt follows. Concurrent identical IDs share a request lock. A
changed request with a reused ID is rejected. After an uncertain response,
Orchard inspects the task or create `external_ref`; it records observed state
without claiming causation, and it never reruns a pending mutation that current
state cannot prove. Beads remains the only mutable task ledger.

## Backup and restore

With the host stopped, copy the whole app data root. This includes config,
owner/workspace credentials, mail Git repositories, and app-owned Beads
SQLite/JSONL files. Preserve file modes, especially credential mode `0600`.
Restore the copy to the same data-root path before starting Orchard. Attached
repositories and external task stores are references and must be backed up
separately; missing references are reported after restore. Relocating the data
root or rebinding external paths is outside this release's restore contract.

## Source snapshots

`vendor/orchard-mail` carries version `0.1.0` from reviewed local commit
`4ea4d304b4c12079cf442af36b2242e45887e10f`. `ORCHARD_SNAPSHOT_SHA256SUMS`
lists every source file. The packaging verifier rejects missing, changed, and
additional files before it compiles the workspace. The unpublished source has
no claimed remote repository URL.
