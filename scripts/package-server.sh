#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
caller=$(pwd -P)
destination=${1:-"$root/dist/orchard-server"}
case "$destination" in
  /*) ;;
  *) destination="$caller/$destination" ;;
esac
parent=$(dirname -- "$destination")
mkdir -p "$parent"
parent=$(CDPATH= cd -- "$parent" && pwd -P)
destination="$parent/$(basename -- "$destination")"
if [ -e "$destination" ] || [ -L "$destination" ]; then
  printf '%s\n' "refusing existing package destination: $destination" >&2
  exit 64
fi
staging=$(mktemp -d "$parent/.orchard-package.XXXXXX")
trap 'rm -rf "$staging"' EXIT HUP INT TERM

cd "$root"
npm --prefix ui ci
npm --prefix ui run build
scripts/verify-vendor-mail.sh

test -x resources/bin/br
test "$(shasum -a 256 resources/bin/br | awk '{print $1}')" = '8c1a0024e35535e49cd1ee97cd432f06e28bf623afbac7fcc6c19f7d3c249ba9'
test -f THIRD_PARTY_NOTICES.txt
test -f vendor/orchard-mail/THIRD_PARTY_LICENSES.txt
test -f third-party/BEADS_THIRD_PARTY_LICENSES.txt
test -f third-party/FRONTEND_THIRD_PARTY_LICENSES.txt
cargo about generate --workspace --frozen --fail -o "$staging/workspace-notices.txt" about.hbs
cmp THIRD_PARTY_NOTICES.txt "$staging/workspace-notices.txt"
cargo about generate --manifest-path vendor/beads-rust/Cargo.toml --no-default-features --frozen --fail --config about.toml -o "$staging/beads-notices.txt" third-party/about-beads.hbs
cmp third-party/BEADS_THIRD_PARTY_LICENSES.txt "$staging/beads-notices.txt"
rm -f "$staging/workspace-notices.txt" "$staging/beads-notices.txt"
cargo build --release --locked -p orchard-server

mkdir -p "$staging/resources/bin" "$staging/notices"
install -m 755 target/release/orchard "$staging/orchard"
install -m 755 resources/bin/br "$staging/resources/bin/br"
install -m 644 LICENSE "$staging/LICENSE"
install -m 644 THIRD_PARTY_NOTICES.txt "$staging/notices/THIRD_PARTY_NOTICES.txt"
install -m 644 vendor/orchard-mail/THIRD_PARTY_LICENSES.txt "$staging/notices/ORCHARD_MAIL_THIRD_PARTY_LICENSES.txt"
install -m 644 third-party/BEADS_THIRD_PARTY_LICENSES.txt "$staging/notices/BEADS_THIRD_PARTY_LICENSES.txt"
install -m 644 third-party/FRONTEND_THIRD_PARTY_LICENSES.txt "$staging/notices/FRONTEND_THIRD_PARTY_LICENSES.txt"

mv "$staging" "$destination"
trap - EXIT HUP INT TERM
printf '%s\n' "$destination"
shasum -a 256 "$destination/orchard" "$destination/resources/bin/br" "$destination/notices/"*
