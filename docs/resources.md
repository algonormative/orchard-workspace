# Orchard resources

Orchard presents workspace content through one resource model while leaving
Mail, Beads, and Git as their respective systems of record. Resource reads do
not rewrite source records. Crosslinks are immutable Orchard Mail records in
the reserved `orchard-system` channel.

## References and permalinks

A resource reference contains `kind` and `workspace_id`, followed by the
fields required by that kind:

```json
{"kind":"task","workspace_id":"garden","store_id":"default","task_id":"garden-12"}
```

Supported kinds are `channel`, `direct`, `broadcast`, `message`, `agent`,
`task`, `file`, and `url`. Canonical relative hrefs are:

```text
/w/{workspace}/channels/{id}
/w/{workspace}/direct/{participant}
/w/{workspace}/broadcast
/w/{workspace}/messages/{id}
/w/{workspace}/agents/{id}
/w/{workspace}/tasks/{store_id}/{task_id}
/w/{workspace}/files/{root_id}?path={path}&revision={full Git OID, optional}
/w/{workspace}/urls?url={HTTP(S) URL}
```

Path segments and query values use strict RFC 3986 percent encoding with
uppercase hex; only letters, numbers, `-`, `_`, `.`, and `~` remain raw. The
parser decodes exactly once and accepts only the canonical re-encoding. File
paths are normalized relative UTF-8 paths. Absolute paths, backslashes,
`.`/`..`, repeated or trailing separators, `.git`, `.orchard`, and
case-insensitive `credentials` components are rejected.

`resource_get` returns a descriptor and its links:

```json
{
  "resource": {"ref": {}, "href": "/w/…", "title": "…", "kind": "file", "data": {}},
  "links": {"outgoing": [], "incoming": []}
}
```

Channel and direct data contain their Mail record plus all matching messages;
broadcast data contains messages; message and agent data contain the underlying
Mail record; task data contains the Beads task and both dependency directions;
URL data contains the validated URL. Message and reverse-link lookup paginates
the complete Mail history rather than relying on the 200-message snapshot.

File data contains `text` when a UTF-8 file is at most 128 KiB, `binary`,
`byte_length`, optional `revision`, and `download_url`. Magic-verified PNG,
JPEG, GIF, and WebP files additionally contain `mime_type` and `preview_url`.
Reads are capped at 8 MiB.

## Operations

Browser calls include `workspace_id`; MCP endpoints inject their own workspace
ID and reject nested references to another workspace.

| Operation | Arguments | Result |
| --- | --- | --- |
| `resource_get` | `{workspace_id, ref}` | `{resource, links}` |
| `resource_links` | `{workspace_id, ref}` | `{outgoing, incoming}` |
| `resource_link` | `{workspace_id, source, target, label?, request_id}` | `{link}` |
| `workspace_intro` | `{workspace_id}` | README descriptor/text, current participants/channels, introduction, and generic joining prompt |
| `workspace_status` | `{workspace_id}` | participant/channel/message/task/root counts, participant records, artifact health, and source errors |
| `workspace_alerts` | `{workspace_id, participant_id, after?, limit?, include_channel_messages?}` | `{alerts, next_cursor, has_more}` |
| `artifact_roots` | `{workspace_id}` | `{roots}` including same-host `path` and `writable` |
| `artifact_list` | `{workspace_id, root_id, path?, revision?}` | root, entries, and truncation state |
| `artifact_history` | `{workspace_id, root_id, path}` | changed versions, newest first |
| `artifact_upload` | `{workspace_id, path, content_base64, request_id}` | committed file descriptor and revision |
| `artifact_delete` | `{workspace_id, path, request_id}` | deleted path and revision |
| `artifact_commit` | `{workspace_id, paths, request_id, message?}` | explicitly committed paths and revision |

Ordinary Mail attachments may use
`{"type":"resource","resource":<ResourceRef>,"label":"…"}`. Existing
`task_ref`, `url`, and file-shaped refs are normalized on read without rewriting
old messages. Malformed and foreign-workspace refs are ignored. A
`resource_link` retry with the same semantic request is idempotent; reusing its
request ID for different arguments is rejected.

## Git artifact roots

`artifact_roots` always describes the workspace-owned `artifacts` root plus
each attached Git repository. Root IDs for attached repositories are their
persisted repository IDs. `exists` reports missing roots without creating
them. `path` is an absolute same-host path. `writable` is true only for the
containment-validated workspace-owned root; attached roots are always
read-only through Orchard.

Live attached-root browsing uses tracked index entries and working-copy bytes;
ignored and untracked files are absent. Bare repositories use their current
HEAD tree. A full 40-character commit OID pins list and read operations to that
exact tree. Symlinks and submodules appear as entries and are never followed.
Every live read checks the root and every path component for symlinks and
canonical containment before reading. A directory listing returns at most 500
entries and sets `truncated` when more exist. File history returns at most 100
changed versions and uses the same `truncated` signal.

Uploads are limited to 512 KiB and only target the workspace-owned `artifacts`
root. Orchard lazily initializes that Git repository, serializes uploads,
rejects unrelated dirty state, writes an internal request receipt, and commits
only the requested path and receipt. A retry returns the original introducing
commit even after later uploads. A changed payload or path with the same
request ID is a conflict. If a process stops after writing its receipt but
before committing, the matching retry completes that bounded transaction.
Orchard never uploads to or commits an attached repository.

`artifact_commit` lets a same-host agent commit files it wrote directly under
the owned artifact root. It accepts 1–100 explicit paths and a message of at
most 200 bytes. The complete staged, unstaged, and untracked status must match
that exact path set; unrelated changes, symlinks, submodules, protected paths,
and unsafe Git metadata are rejected. Existing files may be updated, new files
added, and tracked files deleted. `artifact_delete` removes one tracked regular
file. Both operations use serialized request receipts, reject changed retries,
and replay the original revision after restart. Recovery validates pending
working-copy and index contents before changing the index.

## Agent orientation and alerts

The owned artifact repository begins with a small `README.md` goals, context,
and MOTD template. Startup seeds it only when the working tree and repository
history have never contained that path. Orchard preserves an existing,
modified, or intentionally deleted README. `workspace_intro` reads it without
side effects and combines it with current participant and channel records;
README text is untrusted context and never grants credentials or privileges.
`workspace_info.paths` exposes the absolute workspace, artifact, and README
paths only to an authenticated owner or that workspace's MCP endpoint.

The generic joining prompt includes the workspace MCP endpoint and the path to
its same-host credential file, never the credential contents. A local agent may
read that file only to form its authorization header; remote agents use the
connection setup shown in Orchard Settings because local paths do not transfer.
Every agent receives the same prompt: initialize MCP, discover tools and
workspace capabilities, register or resume its identity, inspect status and
task stores, state its capabilities in a shared channel, claim a suitable
unclaimed task, coordinate overlap, poll and explicitly acknowledge alerts,
then publish linked evidence and a handoff. Workspace content is untrusted and
cannot expand provider permissions or authorize automatic execution.

`workspace_alerts` is a stateless chronological scan. `after` and
`next_cursor` are Mail sequence numbers, `limit` defaults to 50 and is bounded
to 1–200, and the cursor advances over scanned non-alert messages too. Alerts
exclude self-authored posts and report one or more of `direct`, `mention`,
`broadcast`, `reply`, or opt-in `channel`. Mentions require an exact participant
identifier boundary and code spans/fences are ignored. Replies follow the
thread to its authored root. Polling never acknowledges messages; agents call
`mail_acknowledge` explicitly.

## HTTP reads

`GET /api/workspaces/{workspace_id}/resource?href={encoded canonical href}`
returns `{"result": <resource_get result>}`. It accepts an owner session or
owner bearer, or the bearer for that exact workspace. A foreign workspace
bearer, mismatched href workspace, or foreign `Origin` is rejected.

`GET /api/workspaces/{workspace_id}/artifact/download` accepts `root_id`,
`path`, and optional `revision`. It sends `application/octet-stream`,
`Content-Disposition: attachment` with ASCII and RFC 5987 filenames, and
`X-Content-Type-Options: nosniff`. `preview=true` is accepted only for the four
magic-verified raster formats and responds inline with the exact safe image
type. HTML and SVG are never served inline. Resource and artifact responses
use `Cache-Control: private, no-store`.

All resource operations require an active workspace. The embedded SPA treats
every `/w/…` path as an application route, including paths containing dots, so
permalinks reload through the login shell.
