#!/usr/bin/env bash
set -e

cd $(dirname "$0")
DIRNAME="$PWD"

# Init submodules
git submodule update --init --recursive

cd "$DIRNAME/frontend"
trunk build "$@"

cd "$DIRNAME"
cargo build "$@"

# To run: cargo run -- --verbose
