#!/usr/bin/env bash
set -euo pipefail

REMOTE_USER=${REMOTE_USER:-"pi"}
REMOTE_HOST=${REMOTE_HOST:-"192.168.2.2"}
REMOTE_PASS=${REMOTE_PASS:-"raspberry"}
CONTAINER=${CONTAINER:-"blueos-core"}
TARGET=${TARGET:-"armv7-unknown-linux-musleabihf"}
BINARY_NAME=${BINARY_NAME:-"mavlink-server"}
PROFILE=${PROFILE:-"release"}
TMUX_SESSION=${TMUX_SESSION:-"autopilot"}

export SSHPASS="${REMOTE_PASS}"

# Fast path for the bulk binary upload (no ControlMaster — it often caps rsync throughput).
SSH_UPLOAD=(
    -o StrictHostKeyChecking=no
    -o Compression=no
)

# Reuse one connection for the short docker/tmux commands after upload.
SSH_CTL="${TMPDIR:-/tmp}/cross-build-${REMOTE_USER}@${REMOTE_HOST}.sock"
SSH_MUX=(
    -o StrictHostKeyChecking=no
    -o ControlMaster=auto
    -o "ControlPath=${SSH_CTL}"
    -o ControlPersist=120
)

cleanup_ssh() {
    sshpass -e ssh -o "ControlPath=${SSH_CTL}" -O exit "${REMOTE_USER}@${REMOTE_HOST}" 2>/dev/null || true
}
trap cleanup_ssh EXIT

ssh_pi() {
    sshpass -e ssh "${SSH_MUX[@]}" "${REMOTE_USER}@${REMOTE_HOST}" "$@"
}

# Cargo has no --profile debug: omit flags for debug, use --release for release.
# Custom profiles from [profile.*] in Cargo.toml use: cargo build --profile <name>
case "${PROFILE}" in
    release)
        cargo_profile_args=(--release)
        artifact_dir=release
        ;;
    debug | dev)
        cargo_profile_args=()
        artifact_dir=debug
        ;;
    *)
        cargo_profile_args=(--profile "${PROFILE}")
        artifact_dir="${PROFILE}"
        ;;
esac

BINARY=${BINARY:-"target/${TARGET}/${artifact_dir}/${BINARY_NAME}"}

# sccache inside the cross container defaults to /.cache/sccache (HOME=/), which
# is not writable. Point it at a writable host dir that Cross.toml mounts as a volume.
export SCCACHE_DIR="${SCCACHE_DIR:-$HOME/.cache/sccache}"
mkdir -p "${SCCACHE_DIR}"

SKIP_FRONTEND=1 cross build "${cargo_profile_args[@]}" --locked --target "${TARGET}" -p "${BINARY_NAME}"

binary_size="$(du -h "${BINARY}" | cut -f1)"
echo "Uploading ${BINARY} (${binary_size}, profile=${PROFILE})..."

# -av without -z: skip compression on ELF. --whole-file avoids delta checks on re-deploy.
sshpass -e rsync -av --whole-file --info=progress2 \
    -e "ssh ${SSH_UPLOAD[*]}" \
    "${BINARY}" "${REMOTE_USER}@${REMOTE_HOST}:/tmp/${BINARY_NAME}"

ssh_pi "docker cp /tmp/${BINARY_NAME} ${CONTAINER}:/usr/bin/${BINARY_NAME} && \
    docker exec ${CONTAINER} chmod +x /usr/bin/${BINARY_NAME}"

ssh_pi "docker exec ${CONTAINER} tmux send-keys -t ${TMUX_SESSION} C-c"

# ssh_pi "docker exec ${CONTAINER} tmux send-keys -t ${TMUX_SESSION} \
#     'RUST_LOG=debug /usr/bin/${BINARY_NAME}' Enter"

echo "Done. Binary deployed and running in tmux session ${TMUX_SESSION} inside container ${CONTAINER}"
