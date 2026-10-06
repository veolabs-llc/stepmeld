#!/usr/bin/env bash
# Every suite in this repository, one command. Exit status is the
# number of failed suites. No GitHub gate, by choice.
set -u
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 2
failed=0
cargo test --workspace --quiet || failed=$((failed + 1))
cargo clippy --workspace --all-targets --quiet -- -D warnings || failed=$((failed + 1))
cargo fmt --all -- --check || failed=$((failed + 1))
echo "== $failed failed"
exit "$failed"
