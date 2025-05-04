#!/usr/bin/env bash
set -e

DEFAULT_TARGETS=(
    "x86_64-unknown-linux-musl"
    "aarch64-unknown-linux-musl"
    "armv7-unknown-linux-musleabihf"
)
if [[ -n "$TARGETS" ]]; then
    TARGETS=($TARGETS)
else
    TARGETS=("${DEFAULT_TARGETS[@]}")
fi

cd "$(dirname "$0")"
DIRNAME="$PWD"

# Init submodules
git submodule update --init --recursive

# Build the frontend on the host
cd "$DIRNAME/frontend"
trunk build "$@"

# Cross-build for each architecture
cd "$DIRNAME"
for TARGET in "${TARGETS[@]}"; do
    # Workaround from: https://github.com/cross-rs/cross/wiki/FAQ#glibc-version-error
    # This will result in a binary located at "target/build/$TARGET/$TARGET/release/mavlink-server"
    export CARGO_TARGET_DIR=target/build/"$TARGET"

    echo "Building for target: $TARGET"
    cross build --bin=mavlink-server --release --locked --target "$TARGET" "$@"
done

echo "All builds completed successfully!"
