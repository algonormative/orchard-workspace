# Orchard Workspace

Orchard is a local web workspace for coordinating people and already-running
agents through shared mail, Git repositories, and Beads task stores. The
`orchard` executable serves the browser UI and authenticated HTTP/MCP endpoints
on loopback. It does not launch or authenticate an agent or provider.

## Development

Build the browser UI before compiling Rust so the server can embed `ui/dist`:

```sh
npm --prefix ui ci
npm --prefix ui run build
cargo build -p orchard-server --locked
mkdir -p target/debug/resources/bin
cp resources/bin/br target/debug/resources/bin/br
target/debug/orchard --data-dir /tmp/orchard-data
```

The launcher prints the loopback URL, owner access-key file, and access key.
Paste the key into the local login screen. The credential is never put in a URL
or browser storage. `--port PORT` deliberately replaces the persisted loopback
port; without it, Orchard reuses the last port or assigns one on first start.

The finished package has no runtime dependency on Node, Python, Git, a provider
CLI, or `PATH`. Rust/Cargo 1.94.0 is pinned in `rust-toolchain.toml`. The approved
`br` executable is resolved relative to `orchard` as `resources/bin/br`.

## Portable package

```sh
scripts/package-server.sh
```

The script builds `dist/orchard-server/` with the executable, approved task
binary, MIT license, and third-party notices. It rebuilds the UI and verifies
the complete vendored Orchard Mail file set and hashes before compiling. The
package is a local build artifact; signing, notarization, and publication are
outside this repository's current evidence.

Launch a portable package with an external data directory:

```sh
./orchard --data-dir "$HOME/Library/Application Support/Orchard"
```

The bundled Beads source is the reviewed snapshot recorded in
`vendor/beads-rust/ORCHARD_SOURCE.txt`. Its approved commit is
`beff256b491e20508eab0319b23547ff145cfd04`; later upstream sources with
restrictive riders are intentionally excluded. Orchard Mail is pinned at local
commit `4ea4d304b4c12079cf442af36b2242e45887e10f` with no remote repository claim.

See [docs/backend.md](docs/backend.md), [docs/ux.md](docs/ux.md), and
[docs/packaging.md](docs/packaging.md).
