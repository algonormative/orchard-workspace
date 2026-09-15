#!/bin/sh
set -eu

source_url='https://github.com/Dicklesworthstone/beads_rust.git'
source_sha='beff256b491e20508eab0319b23547ff145cfd04'
lock_sha='01b65d23eb9d3c589b7f50bd79d9f0ebc7454e836f0abd05734b838952c5437a'
destination=${1:?usage: scripts/fetch-beads.sh /empty/staging/directory}

if [ -e "$destination" ] && [ "$(find "$destination" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
  printf '%s\n' 'refusing non-empty staging directory' >&2
  exit 64
fi
mkdir -p "$destination"
git -C "$destination" init -q
git -C "$destination" fetch --depth=1 "$source_url" "$source_sha"
git -C "$destination" checkout --detach -q FETCH_HEAD
test "$(git -C "$destination" rev-parse HEAD)" = "$source_sha"
test "$(shasum -a 256 "$destination/Cargo.lock" | awk '{print $1}')" = "$lock_sha"
(cd "$destination" && env RUSTUP_TOOLCHAIN=nightly-2026-02-19 cargo build --locked --release --no-default-features)
printf '%s  %s\n' "$source_sha" "$destination"
shasum -a 256 "$destination/Cargo.lock" "$destination/target/release/br"
