#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
manifest="$root/vendor/beads-rust/Cargo.toml"
lock="$root/vendor/beads-rust/Cargo.lock"
output="$root/third-party/beads-legacy-license-inventory.tsv"
temporary=$(mktemp "${TMPDIR:-/tmp}/orchard-beads-inventory.XXXXXX")
trap 'rm -f "$temporary"' EXIT HUP INT TERM

{
  printf '# source-commit\t%s\n' 'beff256b491e20508eab0319b23547ff145cfd04'
  printf '# cargo-lock-sha256\t%s\n' "$(shasum -a 256 "$lock" | awk '{print $1}')"
  printf 'package\tversion\tdeclared_license\tsource\n'
  cargo metadata --manifest-path "$manifest" --no-default-features --locked --offline --format-version 1 |
    jq -r '.packages[] | [.name, .version, (.license // "NOASSERTION"), (.source // "git+https://github.com/Dicklesworthstone/beads_rust.git#beff256b491e20508eab0319b23547ff145cfd04")] | @tsv' |
    LC_ALL=C sort -u
} > "$temporary"
mv "$temporary" "$output"
trap - EXIT HUP INT TERM
