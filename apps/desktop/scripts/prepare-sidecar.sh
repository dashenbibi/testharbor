#!/bin/sh
set -eu

profile="${1:-debug}"
next="${2:-build}"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/../../.." && pwd)
host=$(rustc -vV | sed -n 's/^host: //p')

if [ "$profile" = "release" ]; then
  cargo build --manifest-path "$workspace_dir/Cargo.toml" --release -p testharbor-process-launcher
  source_bin="$workspace_dir/target/release/testharbor-process-launcher"
else
  cargo build --manifest-path "$workspace_dir/Cargo.toml" -p testharbor-process-launcher
  source_bin="$workspace_dir/target/debug/testharbor-process-launcher"
fi

binary_dir="$script_dir/../src-tauri/binaries"
mkdir -p "$binary_dir"
cp "$source_bin" "$binary_dir/testharbor-process-launcher-$host"

if [ "$next" = "prepare" ]; then
  exit 0
fi
if [ "$next" = "dev" ]; then
  exec npm --prefix "$script_dir/.." run dev
fi
exec npm --prefix "$script_dir/.." run build
