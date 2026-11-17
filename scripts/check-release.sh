#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

if [ "${DRY_RUN:-true}" = true ]; then
    version=Unreleased
else
    version=${NEW_VERSION:?}
fi
bash scripts/release-notes.sh "$version" > /dev/null

python3 tests/service_install.py
cargo test --locked
(
    cd scripts
    bun run typecheck
    bun run test
)
cargo build --release --locked
./target/release/keyboard-debouncer --version
