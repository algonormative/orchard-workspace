#!/bin/sh
set -eu
app=${1:?Usage: scripts/sign-macos-app.sh path/to/Orchard.app}; identity=${APPLE_SIGNING_IDENTITY:?Set APPLE_SIGNING_IDENTITY.}
test -d "$app"; test -x "$app/Contents/MacOS/Orchard"; test -x "$app/Contents/Resources/bin/br"
codesign --force --options runtime --timestamp --sign "$identity" "$app/Contents/Resources/bin/br"
codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
archive=$(mktemp "${TMPDIR:-/tmp}/Orchard-notary.XXXXXX.zip"); trap 'rm -f "$archive"' EXIT HUP INT TERM
ditto -c -k --keepParent "$app" "$archive"
xcrun notarytool submit "$archive" --wait --apple-id "${APPLE_ID:?Set APPLE_ID.}" --team-id "${APPLE_TEAM_ID:?Set APPLE_TEAM_ID.}" --password "${APPLE_APP_SPECIFIC_PASSWORD:?Set APPLE_APP_SPECIFIC_PASSWORD.}"
xcrun stapler staple "$app"; xcrun stapler validate "$app"; codesign --verify --deep --strict --verbose=2 "$app"; spctl --assess --type execute --verbose=4 "$app"
