# Storage and recovery

The mailbox root is a dedicated Git repository. Canonical records are readable
Markdown containing a versioned JSON comment followed by display text:

```markdown
<!-- orchard-mail:v1
{
  "id": "general",
  "name": "general"
}
-->

Default public channel
```

The current schema keeps records under `participants/`, `channels/`,
`messages/`, `acknowledgements/`, and `requests/`, plus `state.md`. Credentials
and process-local session presence are absent from Git. Messages and request
records are immutable. Participant records change on leave, and `state.md`
advances the monotonic message sequence.

On open, Orchard Mail acquires `.git/orchard-mail.lock`, requires a clean index
and worktree, verifies the first-parent commit shape and fixed Orchard Mail
commit identity, verifies tracked paths and HEAD blobs, then checks filename to
record identity, message ID to sequence identity, recipient references, and
next-sequence consistency. These checks detect corruption and out-of-band Git
edits; they are structural integrity checks, not cryptographic author
authentication.

Before changing files, a transaction marker is synced inside `.git`. It names
the starting HEAD and the exact relative paths owned by that mutation. After
files are synced, only those paths are staged and a commit is made. Recovery
has two accepted cases:

1. HEAD is unchanged and every dirty path is listed in the marker. Recovery
   rolls back those paths to HEAD and never commits them.
2. HEAD is the exact single child described by the marker's expected tree and
   commit summary, with a clean worktree. Recovery removes the stale marker and
   keeps the completed commit.

Any unlisted dirty path, invalid marker, unexpected HEAD, merge, record
deletion, or unknown tracked path fails without modifying the repository. A
failure after the commit but before cleanup triggers recovery and rebuilds the
in-memory index before the call returns an error. An exact retry therefore
finds the committed request receipt, and a later send cannot reuse its
sequence. If that recovery itself fails, the live service is poisoned and must
be reopened after human repair.

Record files and the transaction marker are `fsync`ed, as are newly populated
record directories; `.git` is synced after the commit. Tests cover local
process-crash boundaries and the two marker recovery cases. The MVP does not
claim proof against sudden power loss or storage hardware that lies about
flushes: libgit2 controls internal object and ref writes, and Orchard Mail does
not currently perform platform-specific recursive durability verification of
the object database. Git history and request receipts provide recovery after a
normal process crash, rather than a distributed transaction guarantee.

