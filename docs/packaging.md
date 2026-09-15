# Portable server packaging

`scripts/package-server.sh [destination]` creates a self-contained directory.
A relative destination is resolved from the caller's working directory before
the script changes directory. The destination must not already exist.

```text
orchard-server/
├── orchard
├── LICENSE
├── resources/bin/br
└── notices/
    ├── THIRD_PARTY_NOTICES.txt
    ├── ORCHARD_MAIL_THIRD_PARTY_LICENSES.txt
    ├── BEADS_THIRD_PARTY_LICENSES.txt
    └── FRONTEND_THIRD_PARTY_LICENSES.txt
```

The browser assets are compiled into `orchard`; no loose `ui/dist` directory is
needed at runtime. The launcher resolves `br` beside itself and never searches
`PATH`. The data directory remains external and is chosen with `--data-dir`.

The packaging gate rebuilds the UI, validates Orchard Mail's exact vendored file
set plus content hashes, requires the checked-in notices, and uses Cargo's
locked release graph. It also compares freshly generated Rust and Beads notices
to the checked-in copies and verifies the reviewed `br` binary SHA-256. The
result is portable across machines that match the
compiled operating system and architecture. This local artifact is unsigned;
no code-signing, notarization, installer, or remote publication is claimed.

The script requires development-time Node/npm for the UI build and Rust/Cargo
for the executable, plus `cargo-about` 0.9.2 to regenerate and verify Rust
notices. Those tools are absent from the runtime dependency set.

From the package directory, run the bundled server with external state:

```sh
./orchard --data-dir "$HOME/Library/Application Support/Orchard"
```
