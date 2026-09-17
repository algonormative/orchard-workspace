# Orchard

Orchard is a macOS menu bar workspace for coordinating people and already-running
agents through shared mail, Git repositories, and Beads task stores. It serves
its browser UI and authenticated HTTP/MCP endpoints locally. Orchard does not
launch or authenticate an agent or provider.

## Install on macOS

Orchard supports Apple Silicon Macs running macOS 13 or newer.

The first public release is being prepared; there is no signed download yet.
When a release is published:

1. Download `Orchard-<version>-macos-arm64.zip` and its `.sha256` file from the
   [latest GitHub Release](https://github.com/algonormative/orchard-workspace/releases/latest).
2. Optionally verify the download with
   `shasum -a 256 -c Orchard-<version>-macos-arm64.zip.sha256`.
3. Open the ZIP and drag `Orchard.app` to Applications.
4. Open Orchard from Applications. It appears in the menu bar rather than the
   Dock; use its menu to open the workspace.

Public builds will be signed with a Developer ID certificate, notarized by
Apple, and carry a stapled notarization ticket. Orchard stores its default data
in `~/Library/Application Support/Orchard`.

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

The launcher prints the loopback URL. Opening that local URL creates an
HTTP-only browser session automatically after exact loopback Host and Origin
checks; there is no access key to copy into the UI. `--port PORT` deliberately
replaces the persisted loopback port; without it, Orchard reuses the last port
or assigns one on first start.

The finished package has no runtime dependency on Node, Python, Git, a provider
CLI, or `PATH`. Rust/Cargo 1.94.0 is pinned in `rust-toolchain.toml`. The approved
`br` executable is resolved relative to `orchard` as `resources/bin/br`.

## Release packages

To build an unsigned Apple Silicon app locally:

```sh
CARGO_TARGET_DIR=/private/tmp/orchard-ux-target \
  CARGO_INCREMENTAL=0 \
  ORCHARD_VERSION=0.1.0 scripts/package-macos-app.sh
```

Signing, notarization, GitHub Release setup, and the first-release checklist are
documented in [docs/packaging.md](docs/packaging.md).

The server-only package remains available for development and headless use:

```sh
scripts/package-server.sh
```

The script builds `dist/orchard-server/` with the executable, approved task
binary, MIT license, and third-party notices. It rebuilds the UI and verifies
the complete vendored Orchard Mail file set and hashes before compiling. The
server package is an unsigned local build artifact.

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
