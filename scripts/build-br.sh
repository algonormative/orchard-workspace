#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source="$root/vendor/beads-rust"
expected_source='beff256b491e20508eab0319b23547ff145cfd04'
expected_lock='01b65d23eb9d3c589b7f50bd79d9f0ebc7454e836f0abd05734b838952c5437a'
expected_binary='8c1a0024e35535e49cd1ee97cd432f06e28bf623afbac7fcc6c19f7d3c249ba9'

test -f "$source/ORCHARD_SOURCE.txt"
test -f "$source/ORCHARD_SOURCE_SHA256SUMS"
actual=$(mktemp "${TMPDIR:-/tmp}/orchard-beads-actual.XXXXXX")
listed=$(mktemp "${TMPDIR:-/tmp}/orchard-beads-listed.XXXXXX")
trap 'rm -f "$actual" "$listed"' EXIT HUP INT TERM
(
  cd "$root"
  special=$(LC_ALL=C find vendor/beads-rust -path vendor/beads-rust/target -prune -o ! -type f ! -type d -print)
  if [ -n "$special" ]; then
    printf '%s\n' 'Beads vendor snapshot contains symlinks or special entries:' >&2
    printf '%s\n' "$special" >&2
    exit 65
  fi
  LC_ALL=C find vendor/beads-rust -path vendor/beads-rust/target -prune -o -type f ! -name ORCHARD_SOURCE_SHA256SUMS -print | LC_ALL=C sort > "$actual"
  sed -E 's/^[0-9a-f]{64}  //' vendor/beads-rust/ORCHARD_SOURCE_SHA256SUMS | LC_ALL=C sort > "$listed"
  if ! cmp -s "$actual" "$listed"; then
    printf '%s\n' 'Beads vendor file set differs from its reviewed snapshot:' >&2
    diff -u "$listed" "$actual" >&2 || true
    exit 65
  fi
  shasum -a 256 -c vendor/beads-rust/ORCHARD_SOURCE_SHA256SUMS
)
grep -qx "$expected_source" "$source/ORCHARD_SOURCE.txt"
test "$(shasum -a 256 "$source/Cargo.lock" | awk '{print $1}')" = "$expected_lock"
env RUSTUP_TOOLCHAIN=nightly-2026-02-19 cargo build --manifest-path "$source/Cargo.toml" --locked --release --no-default-features
test "$(shasum -a 256 "$source/target/release/br" | awk '{print $1}')" = "$expected_binary"
mkdir -p "$root/resources/bin"
install -m 755 "$source/target/release/br" "$root/resources/bin/br"
shasum -a 256 "$root/resources/bin/br"
