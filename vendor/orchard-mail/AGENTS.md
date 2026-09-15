# Orchard Mail working agreements

This repository is a portable standalone Rust dependency. Keep every source,
test, script, and document usable after copying the repository to another
machine or vendoring the three crates into a different workspace.

- Do not add absolute host paths, references to the parent vault, or assumptions
  about a particular desktop shell.
- Keep `orchard-mail-core` synchronous and independent of HTTP or async
  runtimes. It owns the only mailbox writer lock and never shells out to Git.
- Preserve the public `MailService`, `ToolBackend`, tool names, JSON formats,
  and relative `/mcp` router contract unless the version is intentionally
  changed.
- Credentials and transient presence stay outside the Git mailbox. Cooperative
  names are not provider-authenticated identities, and direct delivery is not a
  privacy boundary.
- A successful durable mutation must have a committed request receipt before it
  is reported as successful. Recovery may roll back only paths explicitly named
  by Orchard Mail's crash marker.
- Tests do not call model APIs, providers, metered services, or the public
  network. Loopback transport tests may bind ephemeral `127.0.0.1` ports.
- Pin direct dependencies exactly and keep `Cargo.lock` current. Regenerate
  `THIRD_PARTY_LICENSES.txt` with the checked-in cargo-about configuration when
  the lockfile changes: `cargo about generate --workspace --frozen --fail -o
  THIRD_PARTY_LICENSES.txt about.hbs`.
- Do not add generated build output under `target/` to source control.
