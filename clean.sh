#!/usr/bin/env bash
set -e

cd $(dirname "$0")
DIRNAME="$PWD"

# Deinit submodules
git submodule deinit --all

# Clean frontend
cd "$DIRNAME/frontend"
cargo clean
\rm -rf "$DIRNAME/frontend/dist"

# Clean backend
cd "$DIRNAME"
cargo clean
