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

## macOS Apple Silicon release

`ORCHARD_VERSION=1.2.3 scripts/package-macos-app.sh [destination]` creates an
unsigned `Orchard.app` with `Contents/MacOS/Orchard`, the approved bundled
`Contents/Resources/bin/br`, and the checked-in notices. Orchard embeds its
server and browser UI in-process. The app contains no Node, Python, Git, Cargo,
or provider runtime. The script verifies the pinned `br` hash and notices before
packaging; `CARGO_TARGET_DIR` may point at an isolated build directory.
Open `Orchard.app` from Finder or with `open Orchard.app`; it stores its default
state in `~/Library/Application Support/Orchard`. Quit an existing `orchard`
CLI server using that data directory before opening the app, because Orchard
permits only one owner for a data directory.

`scripts/sign-macos-app.sh Orchard.app` signs nested `br` and then the app with
hardened runtime, verifies it, submits a ZIP to Apple's notary service, and
staples the accepted ticket. It requires a Developer ID Application identity,
`APPLE_ID`, `APPLE_TEAM_ID`, and an app-specific password; it cannot create
those credentials.

The tagged `vMAJOR.MINOR.PATCH` workflow builds, signs, notarizes, staples, and
publishes `Orchard-MAJOR.MINOR.PATCH-macos-arm64.zip` plus SHA-256 using GitHub
Release. Before enabling it, set the repository variable
`ORCHARD_UPDATE_REPOSITORY` to the exact `owner/repository`, and provide
`APPLE_CERTIFICATE_P12` (base64), `APPLE_CERTIFICATE_PASSWORD`,
`KEYCHAIN_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, and
`APPLE_APP_SPECIFIC_PASSWORD`. This checkout has no Git remote; no repository
is assumed or created. The workflow verifies that the `macos-14` runner is
Apple Silicon before it builds an `arm64` asset.

Updates are checked only when the user invokes the menu action. A distribution
build embeds `ORCHARD_UPDATE_REPOSITORY`, calls that GitHub repository's latest
stable release endpoint once with a ten-second timeout and a 1 MiB response cap,
and accepts only a matching Apple Silicon ZIP. It returns verified release and
download URLs for the UI to open; it never downloads, installs, or schedules an
update. Builds without the variable report updates as unconfigured.
That is expected for local builds until a release repository is chosen and the
application is rebuilt with the variable.
