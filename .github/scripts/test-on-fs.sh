#!/usr/bin/env bash
# Runs the per-filesystem test suites under REMORA_TEST_FS (spec 17.5).
# Usage: test-on-fs.sh <type>, where <type> is what `stat -f -c %T` prints for
# the expected filesystem. Fails before testing if the variables are invalid or
# the directory is on another filesystem, so a missing mount cannot pass by
# testing the runner's root disk.
set -euo pipefail

expected="$1"
case "${REMORA_EXPECT_CLONES-}" in
  0 | 1) ;;
  *)
    echo "REMORA_EXPECT_CLONES must be 0 or 1, got '${REMORA_EXPECT_CLONES-}'" >&2
    exit 1
    ;;
esac
if [ ! -d "${REMORA_TEST_FS-}" ]; then
  echo "REMORA_TEST_FS is not a directory: '${REMORA_TEST_FS-}'" >&2
  exit 1
fi
actual="$(stat -f -c %T "$REMORA_TEST_FS")"
if [ "$actual" != "$expected" ]; then
  echo "REMORA_TEST_FS is on $actual, expected $expected" >&2
  exit 1
fi

echo "testing on $actual at $REMORA_TEST_FS with REMORA_EXPECT_CLONES=$REMORA_EXPECT_CLONES"
exec cargo test --locked --no-fail-fast -p remora-fs -p remora-store -p remora-engine
