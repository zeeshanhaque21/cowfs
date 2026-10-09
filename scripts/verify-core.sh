#!/bin/sh
set -eu
rtk cargo fmt --all --check
rtk cargo clippy --workspace --all-targets -j4 -- -D warnings
rtk cargo test --workspace -j4
rtk cargo doc --workspace --no-deps -j4
echo 'WORKSPACE GATE PASSED'

# ci-trial trial/forced-full (throwaway)
