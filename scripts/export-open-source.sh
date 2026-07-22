#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd)

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <new-export-directory>" >&2
  exit 2
fi

destination=$1
if [ -e "$destination" ]; then
  echo "export destination already exists: $destination" >&2
  exit 2
fi

mkdir -p "$destination"
destination=$(CDPATH= cd -- "$destination" && pwd)

case "$destination/" in
  "$repository_root/"*)
    echo "export destination must be outside the source tree" >&2
    exit 2
    ;;
esac

rsync -a --prune-empty-dirs \
  --exclude-from="$repository_root/.opensourceignore" \
  "$repository_root/" "$destination/"

"$destination/scripts/check-public-tree.sh" "$destination"
echo "open-source export created at $destination"
