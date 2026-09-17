#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
caller=$(pwd -P); destination=${1:-"$root/dist/Orchard.app"}
case "$destination" in /*) ;; *) destination="$caller/$destination" ;; esac
parent=$(dirname -- "$destination"); mkdir -p "$parent"; parent=$(CDPATH='' cd -- "$parent" && pwd -P); destination="$parent/$(basename -- "$destination")"
test ! -e "$destination" && test ! -L "$destination" || { echo "refusing existing package destination: $destination" >&2; exit 64; }
version=${ORCHARD_VERSION:?Set ORCHARD_VERSION to stable x.y.z.}
case "$version" in [0-9]*.[0-9]*.[0-9]*) ;; *) echo 'ORCHARD_VERSION must be stable x.y.z' >&2; exit 64;; esac
case "$version" in *[!0-9.]*|*.*.*.*|.*|*.) echo 'ORCHARD_VERSION must be stable x.y.z' >&2; exit 64;; esac
test "$(uname -m)" = arm64 || { echo 'Apple Silicon packaging requires arm64.' >&2; exit 64; }
staging=$(mktemp -d "$parent/.orchard-app.XXXXXX"); trap 'rm -rf "$staging"' EXIT HUP INT TERM; app="$staging/Orchard.app"
cd "$root"
test "$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "orchard-desktop") | .version')" = "$version"
test "$(jq -r '.version' crates/orchard-desktop/tauri.conf.json)" = "$version"
npm --prefix ui ci; npm --prefix ui run build; scripts/verify-vendor-mail.sh
test -x resources/bin/br
test "$(shasum -a 256 resources/bin/br | awk '{print $1}')" = '8c1a0024e35535e49cd1ee97cd432f06e28bf623afbac7fcc6c19f7d3c249ba9'
for notice in THIRD_PARTY_NOTICES.txt vendor/orchard-mail/THIRD_PARTY_LICENSES.txt third-party/BEADS_THIRD_PARTY_LICENSES.txt third-party/FRONTEND_THIRD_PARTY_LICENSES.txt; do test -f "$notice"; done
cargo about generate --workspace --frozen --fail -o "$staging/workspace-notices.txt" about.hbs; cmp THIRD_PARTY_NOTICES.txt "$staging/workspace-notices.txt"
cargo about generate --manifest-path vendor/beads-rust/Cargo.toml --no-default-features --frozen --fail --config about.toml -o "$staging/beads-notices.txt" third-party/about-beads.hbs; cmp third-party/BEADS_THIRD_PARTY_LICENSES.txt "$staging/beads-notices.txt"
cargo build --release --locked -p orchard-desktop
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/bin" "$app/Contents/Resources/notices"
install -m 755 "${CARGO_TARGET_DIR:-$root/target}/release/orchard-desktop" "$app/Contents/MacOS/Orchard"
install -m 755 resources/bin/br "$app/Contents/Resources/bin/br"; install -m 644 LICENSE "$app/Contents/Resources/LICENSE"
install -m 644 THIRD_PARTY_NOTICES.txt "$app/Contents/Resources/notices/THIRD_PARTY_NOTICES.txt"; install -m 644 vendor/orchard-mail/THIRD_PARTY_LICENSES.txt "$app/Contents/Resources/notices/ORCHARD_MAIL_THIRD_PARTY_LICENSES.txt"; install -m 644 third-party/BEADS_THIRD_PARTY_LICENSES.txt "$app/Contents/Resources/notices/BEADS_THIRD_PARTY_LICENSES.txt"; install -m 644 third-party/FRONTEND_THIRD_PARTY_LICENSES.txt "$app/Contents/Resources/notices/FRONTEND_THIRD_PARTY_LICENSES.txt"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleDisplayName</key><string>Orchard</string><key>CFBundleExecutable</key><string>Orchard</string><key>CFBundleIdentifier</key><string>dev.orchard.desktop</string><key>CFBundleInfoDictionaryVersion</key><string>6.0</string><key>CFBundleName</key><string>Orchard</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>$version</string><key>CFBundleVersion</key><string>$version</string><key>LSMinimumSystemVersion</key><string>13.0</string><key>LSUIElement</key><true/></dict></plist>
EOF
plutil -lint "$app/Contents/Info.plist"; test -x "$app/Contents/MacOS/Orchard"; test -x "$app/Contents/Resources/bin/br"
mv "$app" "$destination"; rm -rf "$staging"; trap - EXIT HUP INT TERM; printf '%s\n' "$destination"; find "$destination" -maxdepth 6 -type f -print0 | xargs -0 shasum -a 256
