#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source="$root/vendor/orchard-mail"
manifest="$source/ORCHARD_SNAPSHOT_SHA256SUMS"
test -f "$manifest"
test -f "$source/ORCHARD_SNAPSHOT.txt"
grep -q '^Commit: 4ea4d304b4c12079cf442af36b2242e45887e10f$' "$source/ORCHARD_SNAPSHOT.txt"

actual=$(mktemp "${TMPDIR:-/tmp}/orchard-mail-actual.XXXXXX")
listed=$(mktemp "${TMPDIR:-/tmp}/orchard-mail-listed.XXXXXX")
trap 'rm -f "$actual" "$listed"' EXIT HUP INT TERM
(
  cd "$source"
  special=$(LC_ALL=C find . ! -type f ! -type d -print)
  if [ -n "$special" ]; then
    printf '%s\n' 'Orchard Mail vendor snapshot contains symlinks or special entries:' >&2
    printf '%s\n' "$special" >&2
    exit 65
  fi
  LC_ALL=C find . -type f ! -name ORCHARD_SNAPSHOT_SHA256SUMS -print | LC_ALL=C sort > "$actual"
  sed -E 's/^[0-9a-f]{64}  //' ORCHARD_SNAPSHOT_SHA256SUMS | LC_ALL=C sort > "$listed"
  if ! cmp -s "$actual" "$listed"; then
    printf '%s\n' 'Orchard Mail vendor file set differs from its reviewed snapshot:' >&2
    diff -u "$listed" "$actual" >&2 || true
    exit 65
  fi
  shasum -a 256 -c ORCHARD_SNAPSHOT_SHA256SUMS
)
