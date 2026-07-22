#!/bin/sh
set -eu

tree=${1:-.}
tree=$(CDPATH= cd -- "$tree" && pwd)
failed=0

for path in \
  AGENTS.md \
  DOGFOOD.md \
  '产品设计.md' \
  'MVP产品设计.md' \
  'MVP技术设计.md' \
  target \
  apps/desktop/node_modules \
  apps/desktop/dist
do
  if [ -e "$tree/$path" ]; then
    echo "private or generated path is present: $path" >&2
    failed=1
  fi
done

if find "$tree" -mindepth 2 -type d -name .git -print -quit | grep -q .; then
  echo "nested Git metadata is present" >&2
  failed=1
fi

if find "$tree" -type f \( \
  -name '*.db' -o -name '*.db-journal' -o -name '*.db-shm' -o \
  -name '*.db-wal' -o -name '*.sqlite' -o -name '*.sqlite3' -o \
  -name '*.log' -o -name 'testharbor-process-launcher-*' \
\) -print -quit | grep -q .; then
  echo "local data, logs, or a generated process launcher is present" >&2
  failed=1
fi

if grep -R -I -n -E \
  --exclude-dir=.git --exclude='Cargo.lock' --exclude='package-lock.json' \
  --exclude='check-public-tree.sh' \
  '/Users/[^/]+/|/home/[^/]+/' "$tree" \
  >/dev/null 2>&1; then
  echo "a user-specific absolute path is present" >&2
  failed=1
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi

echo "public tree checks passed"
