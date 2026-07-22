#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
"$script_dir/prepare-sidecar.sh" release prepare
exec npm --prefix "$script_dir/.." exec tauri build -- --config src-tauri/tauri.bundle.conf.json
