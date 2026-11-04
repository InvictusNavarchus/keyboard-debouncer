#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

cargo test --locked
(
    cd scripts
    bun run typecheck
    bun run test
)
cargo build --release --locked
./target/release/keyboard-debouncer --version
