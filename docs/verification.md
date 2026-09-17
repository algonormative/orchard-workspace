# Local verification — 2026-09-15

This is a local macOS Apple Silicon MVP, with a Rust server and browser UI.
The owner superseded the native Tauri/tray design with web delivery. No native
application, signed installer, remote release, or provider-backed test is claimed.

## Independently checked

- Orchard Mail commit `4ea4d304b4c12079cf442af36b2242e45887e10f`: 17 Rust
  tests, formatting, and Clippy with warnings denied. Tests cover commit/retry
  recovery, conflicting IDs, exclusive writers, index reconstruction, immutable
  history, acknowledgments, and two local official-SDK MCP clients.
- Orchard Workspace: 17 Rust tests (14 host, 3 server), formatting, and Clippy
  with warnings denied. These cover scoped authentication and rotation, origin
  and body limits, restart, backup/restore, unavailable repositories, Beads
  compatibility, external changes, uncertain outcomes, concurrent retries,
  and a task-linked handoff between two local MCP clients.
- Final Playwright fixture: conversation polling, directed-message attribution,
  broadcast routing, task and ledger views, archive behavior, errors, and
  retention of draft/focus/scroll through refresh and reconnect. One workflow
  passed in 36.4 seconds. HTTP and Playwright were used instead of direct browser
  automation.
- HTTP task lifecycle and mail exchange against the embedded server, including
  acknowledgment and handoff references. A read-only SQLite backup of an
  existing Vault Beads store was attached as a disposable copy: all 1,631 IDs,
  titles, and statuses were preserved. The source store was not modified.
- Orchard Mail's vendored source is an exact committed snapshot, with portable
  dependency references and file-set/hash verification. Active Rust dependencies,
  the approved Beads graph, and frontend dependencies have license inventories
  and bundled notices.
- Final release-package Playwright passed owner login, workspace creation,
  task create/show/update/dependencies/close, message submission, and a rendered
  ledger evidence link. Screenshots were reviewed; the corrected store button
  wraps its complete path within the details panel. The package ran from `/tmp`
  with `PATH=/nonexistent`. Both binaries link only to macOS system libraries.

The local package is `dist/orchard-workspace-0.1.0-macos-arm64.tar.gz` with SHA-256
`5eb2c74ea4fbfb3eb3ffd8f0185909acc0193688f0ac9e1447a892395b41e8b0`.
The packaged `orchard` SHA-256 is
`b9e0a8cb6ae0bb57ad27e301ecfc7b2523785e43d63e0f936b90be5529654456`.
These identify this local artifact, not a promise of byte-identical builds on
arbitrary development machines.

The initially proposed Beads commit was rejected for restrictive dependency
license riders. The explicitly recorded replacement is
`beff256b491e20508eab0319b23547ff145cfd04`, still version 0.1.14. The approved
arm64 binary SHA-256 is
`8c1a0024e35535e49cd1ee97cd432f06e28bf623afbac7fcc6c19f7d3c249ba9`.

## Native macOS package — 2026-09-16

An unsigned Apple Silicon `Orchard.app` version 0.1.0 (about 28 MB) was built
at `/private/tmp/orchard-menu-bar/Orchard.app`. The pinned bundled `br` hash and
license-notice gates passed. Forty-six Rust workspace tests in release mode and
nine Playwright fixture tests passed.

With `PATH=/usr/bin:/bin`, the packaged app completed a live loopback smoke for
keyless workspace creation, task work, and upload. A normal macOS Quit stopped
the listener; reopening through LaunchServices retained the workspace. This
does not prove menu interactions, signing, notarization, GitHub CI, or a
clean-machine installation. Release credentials and distribution configuration
remain tracked in `vault-rhtyl`.

## Remaining release evidence

Tracked in Vault epic `vault-1qnss`, release gate `vault-1qnss.7`:

1. Connect actual Claude Code and Codex sessions to the packaged server; exchange
   a task-linked request, response, acknowledgment, and handoff, then restart and
   reconnect. Local SDK clients prove transport behavior, not harness integration.
2. Exercise macOS sleep/wake with those clients connected.
3. Install and run the package on a clean compatible Mac without development
   runtimes. A sanitized-PATH test on the development machine is narrower evidence.

These tests must not be silently replaced with provider calls from automated
tests. Communication backup/restore currently requires a stopped server and
restoring the same data-root path; attached repositories need separate backups.
Process-crash recovery is tested, not arbitrary power-loss durability.
