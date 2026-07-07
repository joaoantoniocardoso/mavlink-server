#!/usr/bin/env bash
# Profile manager-spawned production mavlink-server with an active REST WebSocket
# client so the JSON broadcast path is exercised (matches BlueOS UI load).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=benchmark-common.sh
source "${SCRIPT_DIR}/benchmark-common.sh"

STRACE_BIN=${STRACE_BIN:-/tmp/strace}
WARMUP_SECS=${WARMUP_SECS:-10}
MEASURE_SECS=${MEASURE_SECS:-10}
WS_URL=${WS_URL:-ws://127.0.0.1:8080/v1/rest/ws}
WS_CLIENT=${WS_CLIENT:-}

benchmark_session_begin
benchmark_preflight

pid=$(benchmark_mavlink_server_pids | head -1)
if [[ -z "${pid}" ]]; then
    echo "ERROR: no production mavlink-server running" >&2
    exit 1
fi

if ! ss -ltn 2>/dev/null | grep -q ':8080'; then
    echo "ERROR: web server not listening on :8080" >&2
    exit 1
fi

echo "production pid=${pid}"
benchmark_show_production_config
threads=$(ps -L -p "${pid}" 2>/dev/null | awk 'NR>1' | wc -l)
echo "threads: ${threads}"
echo "binary: $(md5sum /usr/bin/mavlink-server | awk '{print $1}')"

ws_pid=""
cleanup_ws() {
    if [[ -n "${ws_pid}" ]] && kill -0 "${ws_pid}" 2>/dev/null; then
        kill "${ws_pid}" 2>/dev/null || true
        wait "${ws_pid}" 2>/dev/null || true
    fi
}
trap cleanup_ws EXIT

start_ws_client() {
    if command -v websocat >/dev/null 2>&1; then
        websocat -t "${WS_URL}" >/dev/null &
        ws_pid=$!
        return
    fi
    if command -v python3 >/dev/null 2>&1; then
        python3 - "${WS_URL}" <<'PY' &
import asyncio
import sys

try:
    import websockets
except ImportError:
    sys.exit(2)

async def main():
    url = sys.argv[1]
    async with websockets.connect(url) as ws:
        while True:
            await ws.recv()

asyncio.run(main())
PY
        ws_pid=$!
        if ! kill -0 "${ws_pid}" 2>/dev/null; then
            ws_pid=""
            echo "ERROR: need websocat or python3+websockets for WS client" >&2
            exit 1
        fi
        return
    fi
    echo "ERROR: need websocat or python3+websockets for WS client" >&2
    exit 1
}

echo "=== starting REST WebSocket client (${WS_URL}) ==="
start_ws_client
sleep 2
if ! kill -0 "${ws_pid}" 2>/dev/null; then
    echo "ERROR: WebSocket client exited early" >&2
    exit 1
fi
echo "ws client pid=${ws_pid}"

sleep "${WARMUP_SECS}"

echo "=== pidstat -u (usr/sys/cpu), ${MEASURE_SECS}s (WS client active) ==="
pidstat -u -p "${pid}" 2 $((MEASURE_SECS / 2)) 2>/dev/null | awk '/^[0-9][0-9]:/ && NF>=8 && $8 ~ /^[0-9]/ {print}' || true

echo "=== strace -c -f (syscall counts), ${MEASURE_SECS}s (WS client active) ==="
if [[ -x "${STRACE_BIN}" ]]; then
    timeout --signal=INT --kill-after=2 "${MEASURE_SECS}" "${STRACE_BIN}" -c -f -p "${pid}" 2>&1 || true
else
    echo "strace not found at ${STRACE_BIN}"
fi

echo "done"
