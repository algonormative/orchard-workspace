# Packaging and releases

## Public macOS release

The first public release is being prepared; no signed download exists yet.
Public releases will provide a signed and notarized Apple Silicon app as
`Orchard-MAJOR.MINOR.PATCH-macos-arm64.zip`. A user can unzip it, move
`Orchard.app` to Applications, and launch it normally on macOS 13 or newer. The
app runs in the menu bar, so it does not create a Dock icon or a main window at
launch.

The release workflow runs on GitHub's standard `macos-14` Apple Silicon runner.
It uses a release-only Cargo target tree at `/private/tmp/orchard-ux-target`,
with incremental compilation disabled, to stay within the hosted runner's disk
limit. It builds and tests only locked dependency graphs. Automated tests must
not call provider or other metered services.

Before the first release of `algonormative/orchard-workspace`:

1. Set the Actions repository variable `ORCHARD_UPDATE_REPOSITORY` to
   `algonormative/orchard-workspace`. The value is embedded in the app's manual
   update check and the workflow rejects a mismatch with the running repository.
2. Run the workflow manually. This executes the tests and clean-runner package
   build without signing, then retains an explicitly `UNSIGNED` ZIP as a
   seven-day Actions artifact. It does not create a GitHub Release.
3. Export a Developer ID Application certificate as a password-protected `.p12`,
   base64-encode the file, and add the result as `APPLE_CERTIFICATE_P12`.
   An Apple Development identity cannot sign a public direct-download release.
4. Add `APPLE_CERTIFICATE_PASSWORD`, a new ephemeral `KEYCHAIN_PASSWORD`, the
   complete `APPLE_SIGNING_IDENTITY` shown by `security find-identity -v -p
   codesigning`, `APPLE_ID`, `APPLE_TEAM_ID`, and an
   `APPLE_APP_SPECIFIC_PASSWORD` as Actions secrets.
5. Confirm `0.1.0` matches the version in
   `crates/orchard-desktop/Cargo.toml` and
   `crates/orchard-desktop/tauri.conf.json`.
6. When the exact release commit is ready, create and push the annotated tag
   `v0.1.0`.

The tag starts `.github/workflows/macos-release.yml`. The workflow validates the
tag and repository, runs the browser and release-mode Rust tests, imports the
certificate into a temporary keychain, builds the app, signs its nested `br`
binary and app bundle with hardened runtime and a secure timestamp, notarizes a
temporary ZIP, staples and validates the ticket, and assesses the app with
Gatekeeper. Only then does it create a draft GitHub Release with the distribution
ZIP and checksum. It removes temporary signing material even when a step fails.

Download both draft assets on a separate compatible Mac while authenticated to
GitHub. Verify the checksum, install the app in Applications, launch it, create
or open a workspace, quit from the menu, and reopen it. Publish the draft only
after that check passes. CI signing and Gatekeeper assessment do not prove the
complete interactive flow on another Mac, and an unsigned preflight artifact
must never be published as a release.

Do not reuse a version or move a published tag. For a later release, update both
desktop version files, commit the change, and tag that exact commit with the
matching `vMAJOR.MINOR.PATCH` tag.

## Portable server package

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

The tagged `vMAJOR.MINOR.PATCH` workflow verifies that its `macos-14` runner is
Apple Silicon before it builds an `arm64` asset.

Updates are checked only when the user invokes the menu action. A distribution
build embeds `ORCHARD_UPDATE_REPOSITORY`, calls that GitHub repository's latest
stable release endpoint once with a ten-second timeout and a 1 MiB response cap,
and accepts only a matching Apple Silicon ZIP. It returns verified release and
download URLs for the UI to open; it never downloads, installs, or schedules an
update. Builds without the variable report updates as unconfigured.
That is expected for local builds until a release repository is chosen and the
application is rebuilt with the variable.
