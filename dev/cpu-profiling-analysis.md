# mavlink-server CPU profiling analysis

Empirical investigation of why `mavlink-server` uses more CPU than the lean
`mavlink-routerd` (mavlink-router; originally miscalled "mavproxy") for the same
routing workload, measured on the real target (Raspberry Pi / BlueOS) rather
than from theory.

## TL;DR

- **Latest controlled comparison (2026-07-05, stats harness `BENCH_TRIALS=5`
  `BENCH_SAMPLES=15`, n=75/case, single navigator, `dev/run-bench-*.sh`):**
  - **0.9.0** (`2cff88df`) production: **11.85% ±0.35%**; **wip2** (`50ffb99`,
    md5 `95e42435`) production: **8.54% ±0.21%**; **wip3** (`667336a`,
    md5 `6841d042`) production: **8.64% ±0.17%**.
  - **wip2 and wip3 tie on production** (ci95 intervals overlap); both are
    **−28%** vs 0.9.0 (significant).
  - **Zenoh cost (when working):** **+7.50%** on 0.9.0, **+3.55%** on wip2
    (single `multi_thread` runtime). **wip3/wip4d pre-fix slices (~0%) were
    invalid** — zenoh broken on data-plane `current_thread`; see § "Invalidated:
    cheap zenoh on dual-plane".
  - **Lean routing:** wip2 **4.27% ±0.11%**; wip3 **7.33% ±0.14%** regressed.
    wip4d lean **2.91% ±0.09%**‡ **invalidated** — measured `start_lean()`, not
    production layout. See § "Invalidated: --no-web lean tuning".
  - **wip4d post-fix production** (`2ae518a`, md5 `0b4b4809`, 2026-07-06):
    **13.09% ±0.26%**, zenoh slice **+6.02%**, **12 OS threads** (zenoh working).
    Pre-fix **7.48%** was broken zenoh. See § "wip4d post-fix (zenoh-plane)".
  - **`mavlink-routerd` v4 baseline:** **~2.0%** (stable).
  - **Best lean path:** wip4c/wip4d `start_lean()` (~3%). **Production cost**
    with working zenoh on dual-plane is **~13%** — higher than wip2/wip3 headline
    numbers (those had broken zenoh on wip3+).
  - **Invalidated (2026-07-07, `20cc54c`):** all **wip4c/wip4d lean CPU wins**
    (`2.91%`, `3.24%`, strace `clock_gettime` ~520/s) were measured with
    `--no-web` running a **different binary layout** than production — single-thread
    `start_lean()`, 50 ms cached clock, and zenoh stripped from the endpoint list
    instead of only disabling drivers. See § "Invalidated: --no-web lean tuning".
  - **Invalidated (2026-07-07):** a **50 ms unified clock** experiment reported
    production **9.35% → 5.63%** CPU — invalid: no WebSocket clients were
    connected, and 50 ms cache breaks **1 µs** delay/jitter measurement. See §
    "Invalidated: cached-clock shortcut".
  - **Applied (2026-07-07, post-`20cc54c`, md5 `927f47a7`):** replace `sysinfo`
    resource polling with `/proc/self/stat` + `/proc/meminfo`; deduplicate ingress
    `clock_gettime` via `note_ingress()`. Production **7.50% → 6.48%** (WS client
    active, n=30) and **`open`/`getdents64` syscalls eliminated** — see § "Applied:
    sysinfo and ingress clock dedup".
- **Lean routing strace comparison (2026-07-06, `dev/profile-lean-strace.sh`,
  isolated `--no-web` bench copy, 10 s `strace -c` + `pidstat`):** network work
  (`sendto`/`recvfrom`) is **identical** across wip2–wip4d (~300/110 per s). All
  lean regression is **runtime coordination syscalls** — `clock_gettime64`,
  `write` to `eventfd` (tokio reactor wakeups), and (on multi-thread builds)
  `futex`. **wip4c** recovers wip2 syscall rates. See § "Why wip3 regressed lean
  routing" and § "Lean routing strace comparison".
- Steady-state CPU on the **pre-codec** build is **~13–16% of one core**; on
  **`wip2` @ `50ffb99`** (mavlink-codec + cached JSON transcoder, stats harness)
  full production is **8.54% ±0.21%** — see § "0.9.0 vs wip2 vs wip3" and the
  mavlink-codec upgrade section below.
- The dominant cost on **0.9.0** was **the zenoh JSON driver (~6.6%, about half
  of all CPU)** on the pre-codec build; after the codec upgrade on **wip2**
  (`50ffb99`) the zenoh slice drops to **+3.6%** (stats harness). **Do not trust
  wip3/wip4d zenoh slices** until zenoh runs on a `multi_thread` runtime — see §
  "Invalidated: cheap zenoh on dual-plane".
- The rest is **kernel network I/O** (`sendto` + loopback softirq RX +
  netfilter conntrack), which scales with `messages × endpoints`. The TCP
  loopback link is disproportionately expensive.
- **Even with zenoh disabled, mavlink-server still costs more than
  mavlink-routerd** — the gap depends on what else is running:
  - **No zenoh, with web/REST** (`:8080` + default REST driver): **~2.7×**
    (~6.5% vs ~2.4% task-clock).
  - **No zenoh, no web/REST** (`--no-web`, true 1:1 routing): **~2.3×**
    (wip2 lean **4.49%** vs routerd **1.95%**, stats harness n=75).
  Structural reasons in both cases: multi-threaded runtime wakes a task per
  endpoint (`futex`/context-switch churn), musl has no VDSO so `clock_gettime` is
  a real syscall (~3,400×/s), and a `sysinfo` metrics loop scans
  `/proc/<pid>/task/...` (~290×/s). The web/REST stack accounts for roughly
  **~1.5–2.5% CPU** on top of the lean routing base.
- **Switching to a single `current_thread` reactor fixes lean routing on the
  wip3+ data path** — but only when combined with the wip4c hot-path fixes
  (50 ms clock refresher, direct `DataPlane::register_sink_with_origin`). wip4c
  lean: **~3.0% CPU, 1 thread, 520 `clock_gettime`/s, 17 `eventfd write`/s** —
  at or below wip2. Dual-plane `current_thread` without those fixes (wip3 data
  plane, wip4b both planes) still regresses. The early wip2-only experiment
  (4.30 → 3.45%) used one `current_thread` on the pre-dual-plane code path.
  **Invalidated 2026-07-07** — those lean numbers used `--no-web` tuning that is
  no longer part of the product; see § "Invalidated: --no-web lean tuning".
- Router bookkeeping (stats, etc.) is **not** a meaningful fraction. This is why
  the earlier `RwLock`→atomics stats change produced no measurable runtime
  difference — it optimized a part that was never hot.

## Environment

- Host: Raspberry Pi, `armv7l`, 4 cores, kernel `5.10.33-v7l+`.
- Container: `blueos-core` (Debian 12).
- Binary: `armv7-unknown-linux-musleabihf`, `release` profile (stripped),
  **musl static**.
- Launched by `ardupilot_manager` (`MAVLinkServer.py`), master endpoint
  `udps:127.0.0.1:8852` fed by the ArduPilot navigator, fanning out to:
  `zenoh:0.0.0.0:7117`, `zenohraw:0.0.0.0:7117`, `udpc:127.0.0.1:14000`,
  `udpc:192.168.2.1:14550`, `udps:127.0.0.1:14001`, `udps:0.0.0.0:14660`,
  `udps:0.0.0.0:11001`, `tcps:127.0.0.1:5777`, plus a web server on `:8080`.
- Tools: `pidstat` (in-container). `strace` copied from Pi host to
  `/tmp/strace` in-container for profiling scripts (`perf` unavailable — no DNS
  to apt in `blueos-core`).
- **Branch / commits:**
  - `f72519cc9b2fcf89f3fc86f5f17c6add42bdd589` — adds `--no-web` and
    `dev/benchmark-lean-router.sh` (2026-07-04).
  - `4e24053350e79d78f66c0bed50819a319252ea3c` — mavlink-codec wip2 JSON
    transcoder + cached `Protocol` representation; `dev/benchmark-common.sh`
    and `dev/benchmark-codec-impact.sh` (2026-07-04).
  - `#[tokio::main]` switched to `flavor = "current_thread"` in `src/main.rs`
    (2026-07-04); benchmark harness switched to `SIGSTOP`-based manager isolation
    and always-fresh bench binary snapshot (md5 `88eb22…`).
  - **`wip2` @ `50ffb99`** (mavlink-codec + cached `Protocol` JSON, md5
    `95e42435…`), **`0.9.0`** (built from tag, md5 `2cff88df…`), and **`wip3` @
    `667336a`** (PlaneRuntimes dual-plane, md5 `6841d042…`) — head-to-head
    re-measured 2026-07-05 with the stats harness (n=75/case); see §
    "0.9.0 vs wip2 vs wip3".
  - **`wip4` @ `ab20b48`** (unified `multi_thread` runtime when `--no-web`, md5
    `d691a68d…`), **`wip4b` @ `ad6b56b`** (dual `current_thread` planes, md5
    `210c1fc1…`), and **`wip4c` @ `fb6f8a9`** (single `current_thread` lean +
    hot-path fixes, md5 `51eb8734…`).
  - **`wip4d` @ `a16b3e3`** (production control `current_thread`, md5
    `a2cdd520…`) + **`2ae518a`** (zenoh-plane `multi_thread`, md5 `0b4b4809…`)
    — full discrimination 2026-07-06 via `dev/run-bench-wip4d.sh`; see §
    "wip4d full discrimination" and § "wip4d post-fix (zenoh-plane)".
  - **`20cc54c`** (2026-07-07) — removes `--no-web` runtime/clock tuning;
    `--no-web` only disables web server, default REST driver, and zenoh drivers;
    `now_micros()` reads the clock directly for microsecond delay/jitter stats.
  - **Post-`20cc54c` syscall fixes** (2026-07-07, md5 `927f47a7`) — `sysinfo`
    removed from resource metrics; ingress stats reuse `message.timestamp`. See §
    "Applied: sysinfo and ingress clock dedup".
  - Deploy with `./cross_build_and_run.sh`; run `benchmark_force_consolidate`
    after deploy if the manager was stopped by the script's `C-c`.

## Runtime layout

Most measurements below were taken with the original
`#[tokio::main(flavor = "multi_thread")]` (no explicit worker count → 4 worker
threads, one per core). Threads observed (`pidstat -t`): `mavlink-server`
(main), 4× `tokio-runtime-w`, and zenoh's `app-0`, `net-0`, `tx-0`, `rx-0`,
`rx-1` (10 threads total; 12 when both zenoh drivers are active).

**wip2** uses a single `multi_thread` runtime (`#[tokio::main]`). An early
experiment switched wip2 to `current_thread` on one runtime (see
`current_thread` section); lean dropped to **3.45%**.

**wip3 through wip4d @ `2ae518a`** use `PlaneRuntimes` (`src/lib/runtime.rs`)
with separate data, control, and zenoh handles. Historical variants below
predate **`20cc54c`**, which removed the `--no-web`-specific `start_lean()` path.

### Current layout (`wip4d` @ `20cc54c` and later)

**Production and `--no-web` share the same runtime topology.** `--no-web` only
skips the `:8080` web server, the default REST/WebSocket driver, and zenoh
drivers (filtered in `cli::endpoints()`). Both modes call `start_dual_plane()`:

| Plane | Runtime | OS thread |
|-------|---------|-----------|
| Data | `current_thread` | `data-plane` |
| Control | `current_thread` | main |
| Zenoh | `multi_thread` | `zenoh-plane` |

**12 threads** with working zenoh (zenoh internal workers included). With
`--no-web`, zenoh drivers are not started but the zenoh-plane thread still
exists — same layout as production, fewer active drivers.

**Timestamps:** `now_micros()` (`src/lib/time.rs`) calls `SystemTime::now()`
on every use — no background cache — so message timestamps and delay/jitter
stats retain **microsecond resolution**. On musl each call is a real
`clock_gettime` syscall (no VDSO).

### Historical layout (invalid for lean comparison after `20cc54c`)

**wip3+** previously used different layouts when `--no-web` was set. Lean
`--no-web` thread counts by variant ( **invalid post-`20cc54c`** ):

| Branch | Data plane | Control plane | Lean threads |
|--------|------------|---------------|--------------|
| wip3 | `current_thread` on `data-plane` OS thread | `multi_thread` (4 workers) | 6 |
| wip4 | unified with control when `--no-web` | same `multi_thread` | 5 |
| wip4b | `current_thread` on `data-plane` OS thread | `current_thread` on main | 2 |
| wip4c | single `current_thread` (`start_lean`) | same handle as data | **1** |
| wip4d | same as wip4c (`start_lean`) | same handle as data | **1** |

Production (`--no-web` false) thread counts by variant:

| Branch | Data plane | Control plane | Prod threads |
|--------|------------|---------------|--------------|
| wip3 / wip4c | `current_thread` on `data-plane` OS thread | `multi_thread` (4 workers) | **6** |
| **wip4d** | `current_thread` on `data-plane` OS thread | **`current_thread` on main** + **`multi_thread` zenoh-plane** | **12** (prod, zenoh active) |

Where a table below is tagged "multi-thread" it predates the wip3 split; strace
tables in § "Lean routing strace comparison" supersede thread-count assumptions
for dual-plane branches.

## Measurements

### 1. Overall CPU (`pidstat`, steady state)

```
%usr ~7.7   %system ~8.0   %CPU ~15.7
```

Roughly half the CPU is spent in the kernel. Per-thread load is spread thinly
across the 4 tokio workers with high `%wait` and frequent CPU migration —
scheduler churn relative to the actual work.

### 2. Scheduler churn (`perf stat`, 10s)

```
task-clock         ~1.6 s  (~16% of one CPU)
context-switches   ~13,150 (~1,315/s)
cpu-migrations     ~129
page-faults        ~2,075
(hardware cycles/instructions: not available in container — no PMU access)
```

~1,300 context switches/sec to route ~500 msgs/sec: high coordination overhead
for a light workload.

### 3. Syscall counts (`strace -c -f`, 10s; read the *counts*, not the
ptrace-inflated times)

| syscall           | calls   | notes |
|-------------------|---------|-------|
| `clock_gettime64` | 34,019  | ~3,400/s as **real** syscalls — musl has no VDSO fast path on this target; glibc `mavlink-routerd` pays ~nothing |
| `sendto`          | 5,265   | the actual message fan-out (real work) |
| `futex`           | 3,105 (569 err) | cross-thread wakeups / lock contention (tokio work-stealing + broadcast fan-out) |
| `open`            | 2,932 (1,992 ENOENT) | periodic filesystem scan, mostly failing — wasteful |
| `getdents64`      | 1,838   | directory enumeration (part of the same scan) |
| `recvfrom`        | 1,133   | inbound from autopilot |
| `writev`/`write`  | ~1,340  | logging / output |

### 4. On-CPU profile (`perf record -g -F 999`, ~2,000 samples)

Kernel vs user: **~50% `[kernel.kallsyms]`**, ~50% user (`mavlink-server`,
unsymbolized because stripped).

Top on-CPU paths (self/children, kernel):

```
sock_sendmsg / inet_sendmsg ................ ~22%   (sendto)
  tcp_sendmsg (+push/write_xmit/transmit) .. ~12%   (tcps:5777 loopback link)
  udp_sendmsg (+ip_send_skb/udp_send_skb) .. ~10%   (UDP fan-out)
softirq RX (net_rx_action/process_backlog/
  __netif_receive_skb/ip_rcv/ip_deliver) ... ~12%   (loopback delivery side)
nf_conntrack + nf_tables + nf_nat .......... ~2–3%  (netfilter on 127.0.0.1)
__sched_text_start ......................... ~4%    (scheduler)
```

Loopback traffic is billed twice: once on the TX stack and again as softirq RX
on the receiving socket, plus conntrack on every packet.

### 5. Controlled A/B — worker threads (identical live load)

| workers | %CPU  | ctx-sw/10s | migrations |
|---------|-------|------------|------------|
| 4 (default) | 13.75 | 8,358  | 72 |
| 1           | 12.06 | 14,935 | 36 |
| 2           | 12.81 | 8,636  | 36 |

This tuned `worker_threads` **on the multi-thread flavor** — going to 1 worker
saved only ~1% CPU and *increased* context switches, because the multi-thread
runtime still keeps its work-stealing pool and driver/blocking threads. That is
**not** the same as switching the runtime *flavor*: moving to `current_thread`
(see that section) cut context switches ~3× and CPU ~0.85% absolute on the lean
build. The lever is the flavor, not the worker count.

### 6. Controlled A/B — endpoint attribution (identical live load)

| Config                              | %CPU  | threads |
|-------------------------------------|-------|---------|
| full (all endpoints)                | 13.4  | 12 |
| − zenoh (both JSON + raw)            | 6.6   | 5  |
| − TCP (`tcps:5777`)                  | 10.9  | 12 |
| UDP outputs only                    | 5.5   | 5  |
| master only (RX, no outputs)        | 3.9   | 5  |

Split of the two zenoh drivers (over the no-zenoh base of ~6.2%):

| Config                | %CPU  | delta vs base |
|-----------------------|-------|---------------|
| base, no zenoh        | 6.2   | —             |
| base + zenoh **JSON** | 12.8  | **+6.6**      |
| base + zenoh **raw**  | 7.7   | +1.5          |

*Pre-codec figures above; post-codec full production is ~11.0% — see below.*

## mavlink-codec upgrade (`wip2`)

Early measurements at `4e24053` (2026-07-04, single-sample harness) showed full
production dropping from ~12.8% to ~11.0%. **Stats harness re-measurement at
`50ffb99` (2026-07-05, n=75):** production **8.54% ±0.21%**, zenoh slice
**+3.55%** (down from +7.50% on 0.9.0). See § "0.9.0 vs wip2 vs wip3" for the
full three-way table.

Re-measured 2026-07-04 after upgrading to **mavlink-codec `wip2`** with a
descriptor-based JSON transcoder and a cached `MAVLinkJSON` representation on
`Protocol` (shared by the zenoh JSON driver and REST/WebSocket broadcast).

Binary: `armv7-unknown-linux-musleabihf` release, md5
`523f72569dd6f51fba15008fb538113e` @ `4e24053` (multi-thread). Measured with
`dev/benchmark-codec-impact.sh` (8 s warmup, 10 s `pidstat` + `perf stat`). The
script measures the manager-spawned production instance first (no duplicate),
then isolates bench cases.

> Isolation method (updated): the harness originally bind-mounted an `exit 1`
> stub over `/usr/bin/mavlink-server`. That made `ardupilot_manager` treat its
> child as crashed, triggering `kill_ardupilot` (killing the navigator source)
> and thrashing — sometimes leaving *two* servers. It now **pauses the manager
> with `SIGSTOP`** (resumed with `SIGCONT` afterwards), which freezes its
> supervision loop while leaving the navigator running, guaranteeing exactly one
> mavlink-server during a run and never overwriting the real binary.

| Config | pidstat %CPU | task-clock / 10s | ctx-sw/10s | threads |
|--------|--------------|------------------|------------|---------|
| full production (routing + zenoh + web/REST) | 10.05 | 1099 ms (**11.0%**) | 7330 | 10 |
| no zenoh (routing + web/REST) | 6.95 | 754 ms (**7.5%**) | 6146 | 5 |
| lean (`--no-web`) | 4.30 | 465 ms (**4.7%**) | 5054 | 5 |
| `mavlink-routerd` v4 | 1.80 | 206 ms (**2.1%**) | 1032 | 1 |

### Impact vs pre-codec baselines

| Config | pre-codec | `4e24053` | Δ |
|--------|-----------|-----------|---|
| full production | ~13.4–15.7% | **~11.0%** | **~2–4% absolute (~18–25%)** |
| no zenoh + web/REST | ~6.5–6.6% | **~7.5%** | ~1% (within run-to-run noise) |
| lean `--no-web` (`f72519c`) | ~4.0–5.1% | **~4.7%** | **~0.4–0.5% (~8–10%)** |
| mavlink-routerd (control) | ~2.2–2.6% | **~2.1%** | unchanged |
| lean / routerd ratio | ~1.8–2.0× | **~2.2×** | structural gap unchanged |

### What changed

- **Zenoh JSON + REST/WebSocket** share a cached transcoder on `Protocol` — JSON
  is materialized once per message and reused. Full-production user CPU drops
  (~5.25% usr / 4.80% sys vs the earlier ~50/50 split).
- **Implied zenoh+JSON cost** falls from **~+6.6%** (pre-codec endpoint
  attribution) to **~+3.5%** (11.0% full − 7.5% no-zenoh on this run).
- **Lean routing base is barely affected** — the codec work does not address
  broadcast fan-out, musl `clock_gettime`, or `/proc` metrics scanning. The
  ~2× gap vs `mavlink-routerd` on `--no-web` remains.

Commits on `wip2` leading to `4e24053`:

```
96dcc46 cargo: src: lib: Upgrade mavlink stack to 0.18 and mavlink-codec wip2
b314647 src: lib: protocol: Add cached MAVLinkJSON transcoder representation
83bf09f src: lib: drivers: zenoh: json: Use descriptor transcoder for JSON driver
4e24053 src: lib: drivers: rest: Reuse Protocol JSON cache for WebSocket broadcast
```

## Single-thread runtime (`current_thread`)

Re-measured 2026-07-04 after switching `#[tokio::main]` to
`flavor = "current_thread"` in `src/main.rs`. This is a **different lever** than
the worker-count A/B in §5: that test set `worker_threads` on the
*multi-thread* runtime, which still spawns a work-stealing pool plus separate
driver/blocking threads (and, counter-intuitively, had *more* context switches
at 1 worker). `current_thread` runs the whole reactor on a single OS thread —
no work-stealing, no cross-thread wakeups.

Lean (`--no-web`, no zenoh), identical live navigator load, single-instance via
`dev/benchmark-lean-router.sh` (SIGSTOP-isolated manager). Binary md5
`88eb22cad98bf8593c74b0c9a0eced2f`.

| Config | pidstat %CPU | usr | sys | task-clock / 10s | ctx-sw/10s | migrations | threads |
|--------|--------------|-----|-----|------------------|------------|------------|---------|
| lean **multi-thread** (`523f72…`) | 4.30 | 0.60 | 3.70 | 440 ms (~4.4%) | 4625 | 36 | 5 |
| lean **single-thread** (`88eb22…`) | **3.45** | 0.60 | 2.85 | 416 ms (~4.2%) | **1576** | **3** | **1** |
| `mavlink-routerd` v4 | 1.80 | 0.10 | 1.70 | 207 ms (~2.1%) | 1058 | 8 | 1 |

- **Context switches fell ~3×** (4625 → 1576) and **migrations ~12×** (36 → 3):
  eliminating the multi-thread runtime removes the cross-thread wakeup churn that
  dominated the earlier scheduler overhead.
- **Total CPU dropped 4.30 → 3.45%**, entirely in **sys** (3.70 → 2.85). User CPU
  is unchanged (0.60) — the per-message routing work is the same.
- **Gap to `mavlink-routerd` narrows** from ~2.4× to **~1.9×**. The residual is
  still sys-dominated (2.85 vs 1.70): musl's missing VDSO `clock_gettime`,
  loopback TCP softirq, and `/proc` metrics scanning remain.

This supersedes §5's "thread count is a non-lever" conclusion for the
`current_thread` flavor specifically: switching runtime *flavor* is a real lever;
tuning `worker_threads` on the multi-thread flavor was not.

## 0.9.0 vs wip2 vs wip3 (2026-07-05)

Re-measured after a **host restart** with a **single** `ardupilot_navigator`
instance (Linux firmware, master `udps:127.0.0.1:8852`). Earlier runs under
memory pressure or with orphaned navigators (from benchmark `restore` not killing
navigator children) inflated CPU and are **discarded**.

Harness: `dev/run-bench-090.sh` / `dev/run-bench-wip2.sh` / `dev/run-bench-wip3.sh`
(codec-impact + lean-router under one `flock` lock). Settings: `BENCH_TRIALS=5`,
`BENCH_SAMPLES=15`, `BENCH_INTERVAL_SECS=2`, `WARMUP_SECS=10` → **n=75**
pidstat samples per case. Production is measured on the manager-spawned PID only;
isolated cases use a fresh copy at `/tmp/mavlink-server-bench`. `perf`/`strace`
were unavailable in-container (no DNS to apt); metrics are **pidstat %CPU** only.

Logs: `/tmp/bench-090-full.log`, `/tmp/bench-wip2-full.log`,
`/tmp/bench-wip3-full.log` (inside `blueos-core`).

### Results (mean ± ci95, n=75)

| Config | **0.9.0** (`2cff88df`) | **wip2** (`95e42435` @ `50ffb99`) | **wip3** (`6841d042` @ `667336a`) |
|--------|------------------------|-----------------------------------|-----------------------------------|
| full production (routing + zenoh + web/REST) | **11.85% ±0.35%** | **8.54% ±0.21%** | **8.64% ±0.17%** |
| no zenoh (routing + web/REST) | **4.35% ±0.15%** | **4.99% ±0.09%** | **8.29% ±0.16%** |
| lean (routing only) | 4.55% ±0.18%† | **4.27% ±0.11%** (`--no-web`) | **7.33% ±0.14%** (`--no-web`) |
| `mavlink-routerd` v4 (codec-impact) | **2.09% ±0.09%** | **1.89% ±0.08%** | **2.06% ±0.09%** |
| lean head-to-head (`benchmark-lean-router.sh`) | **4.45% ±0.18%** | **4.49% ±0.12%** | **7.33% ±0.15%** |
| `mavlink-routerd` v4 (lean-router) | **2.08% ±0.08%** | **1.95% ±0.07%** | **1.93% ±0.10%** |

† **0.9.0 has no `--no-web` flag**; the codec-impact "lean" row still runs
web/REST and matches no-zenoh within ci95. wip2 and wip3 lean use `--no-web`.

### Statistical significance

Comparing normal-approximation **ci95** intervals (`mean ± ci95`):

| Comparison | 0.9.0 | wip2 | wip3 | Significant? |
|------------|-------|------|------|--------------|
| Production | 11.50–12.20% | 8.33–8.75% | 8.47–8.81% | 0.9.0 vs both: **Yes**; wip2 vs wip3: **No** |
| No zenoh / routing+web | 4.20–4.50% | 4.90–5.08% | 8.13–8.45% | wip2 vs 0.9.0: **No**; wip3 vs both: **Yes** |
| True lean (`--no-web`) | — | 4.16–4.38% | 7.19–7.47% | wip2 vs wip3: **Yes**; wip2 vs 0.9.0 no-zenoh: **No** |
| `mavlink-routerd` baseline | 2.00–2.16% | 1.88–2.02% | 1.83–2.03% | **No** — all overlap |
| Zenoh slice (prod − no-zenoh) | +7.35–7.65% | +3.44–3.66% | +0.19–0.51% | All pairs: **Yes** |

Fair lean anchor for 0.9.0: no-zenoh row (4.35%) — web still enabled, no
`--no-web` flag.

All isolated cases used preflight with exactly **one** live navigator.

### Incremental decomposition

**0.9.0 production (11.85%)**

```
mavlink-routerd reference           ~2.08%
  + routing fan-out + web/REST      +2.27%   →  no-zenoh = 4.35%
  + zenoh + zenohraw                 +7.50%   →  production = 11.85%
```

| Component | ~CPU% | Share of production |
|-----------|-------|---------------------|
| Routerd baseline | ~2.1% | 18% |
| Routing + web (above routerd) | ~2.3% | 19% |
| **Zenoh stack** | **~7.5%** | **63%** |
| **Total** | **11.9%** | 100% |

Lean vs `mavlink-routerd`: **4.45% vs 2.08%** → **+2.37%** (+2.1×).

**wip2 production (8.54%)**

```
mavlink-routerd reference           ~1.95%
  + routing fan-out (no web)        +2.32%   →  lean = 4.27%
  + web/REST                        +0.72%   →  no-zenoh = 4.99%
  + zenoh + zenohraw                 +3.55%   →  production = 8.54%
```

| Component | ~CPU% | Share of production |
|-----------|-------|---------------------|
| Routerd baseline | ~2.0% | 23% |
| Routing core (lean − routerd) | ~2.3% | 27% |
| Web/REST | ~0.7% | 8% |
| **Zenoh stack** | **~3.6%** | **42%** |
| **Total** | **8.5%** | 100% |

Lean vs `mavlink-routerd`: **4.49% vs 1.95%** → **+2.54%** (+2.3×).

**wip3 production (8.64%)**

```
mavlink-routerd reference           ~2.0%
  + routing fan-out (no web)        +5.33%   →  lean = 7.33%
  + web/REST                        +0.96%   →  no-zenoh = 8.29%
  + zenoh + zenohraw                 +0.35%†   →  production = 8.64%
```

†Likely **invalid** — zenoh probably not running on data-plane `current_thread`;
see § "Invalidated: cheap zenoh on dual-plane".

| Component | ~CPU% | Share of production |
|-----------|-------|---------------------|
| Routerd baseline | ~2.0% | 23% |
| Routing core (lean − routerd) | ~5.3% | 62% |
| Web/REST | ~1.0% | 11% |
| **Zenoh stack** | **~0.4%** | **4%** |
| **Total** | **8.6%** | 100% |

Lean vs `mavlink-routerd`: **7.33% vs 1.93%** → **+5.40%** (+3.8×).

### What changed between versions

| Metric | 0.9.0 | wip2 | wip3 | **wip4d‡** | Interpretation |
|--------|-------|------|------|------------|----------------|
| Production | 11.85% | 8.54% | 8.64%† | **13.09%** | wip3/wip4d† had broken zenoh; post-fix is true production cost |
| Zenoh slice | +7.5% | +3.6% | +0.4%† | **+6.0%** | wip4d zenoh working; higher than wip2 (dual-plane + 3 runtimes) |
| True lean (`--no-web`) | ~4.4%‡ | **4.27%** | 7.33% | **2.91%** | wip4d beats wip2 and wip3 on lean |
| Lean − routerd gap | +2.4% | +2.5% | +5.4% | **+1.1%** | wip4d closes routerd gap |
| `mavlink-routerd` | 2.08% | 1.95% | 1.93% | **1.79%** | Reference stable |
| Prod OS threads | — | 5 | 6 | **12** | wip4d: data + control + zenoh-plane + zenoh internal |

†wip3 production/zenoh likely invalid (zenoh broken on data-plane CT). ‡wip4d post-fix
(`2ae518a`, md5 `0b4b4809`); pre-fix broken-zenoh run was 7.48%.

†0.9.0 lean-router with web (4.45%) or no-zenoh (4.35%).

**Key finding:** wip3's PlaneRuntimes split did **not** fix zenoh — zenoh was
**broken** on the data-plane `current_thread` runtime. **wip4c** fixes lean;
**2ae518a** zenoh-plane fix restores zenoh at **+6.0%** CPU. True wip4d
production is **13.09%** — **not** comparable to wip2/wip3 headline numbers
(wip3+ had broken zenoh in benchmarks).

### Fix confirmed (2026-07-06, `2ae518a`)

Re-benchmark (`dev/run-bench-wip4d.sh`, log `/tmp/bench-wip4d-zenoh-fix.log`):

| Signal | Pre-fix (`a2cdd520`) | Post-fix (`0b4b4809`) |
|--------|----------------------|------------------------|
| Production CPU | 7.48% | **13.09% ±0.26%** |
| Zenoh slice | ~0% | **+6.02%** |
| OS threads | 2 | **12** |
| Lean (`--no-web`) | 3.24% | **2.91% ±0.09%** (still valid) |

Zenoh is functioning; production CPU revised **+5.6%** vs the broken run.

## Invalidated: cheap zenoh on dual-plane (2026-07-06)

**Discovery:** The near-zero zenoh CPU slices on **wip3** (+0.35%) and **wip4d**
(+0.02%) were **not** because dual-plane eliminated zenoh overhead. **Zenoh was
not functioning.** The zenoh Rust stack requires a Tokio **`multi_thread`**
runtime; dual-plane layouts placed zenoh drivers and session setup on the
**data-plane `current_thread`** runtime.

### Root cause (code path)

1. **`HubActor::add_driver`** spawns all drivers on `data_handle` (data-plane
   `current_thread` since `f87baab`).
2. **`drivers/zenoh/session.rs`** opens the shared session with bare
   `tokio::spawn(open_zenoh_session(...))` on whatever runtime is current — the
   data-plane `current_thread` reactor.
3. Zenoh drivers call `session().await` from that same runtime; `zenoh::open` and
   its internal tasks expect concurrent worker threads.

On **wip2** (single `multi_thread` `#[tokio::main]`), zenoh works and costs
**+3.55%**. On **wip3+** dual-plane, zenoh silently fails to operate while still
being configured in the production endpoint list.

### Evidence the benchmarks were measuring broken zenoh

| Signal | Expected (working zenoh) | Observed (wip3/wip4d) |
|--------|------------------------|------------------------|
| Zenoh slice (prod − no-zenoh) | +0.4–3.6% (wip2-scale) | **~0%** |
| Production ≈ no-zenoh CPU | No | **Yes** (wip4d: 7.48% vs 7.46%) |
| OS thread count (production) | ~8–12 (zenoh internal threads) | **2** on wip4d strace |
| strace `write`/s (production) | Higher with zenoh pub/sub | 551/s — no zenoh worker churn |

The **2-thread** wip4d production profile (`data-plane` + main) was the smoking
gun: a working zenoh session adds `app-0`, `net-0`, `tx-0`, `rx-*` threads (see
§ "Runtime layout" pre-split observation: 10–12 threads with zenoh).

### What remains valid vs invalidated

| Measurement | Status |
|-------------|--------|
| wip2/wip3/wip4d **lean** (`--no-web`, no zenoh endpoints) | **Invalidated** — see § "--no-web lean tuning" |
| wip4c/wip4d lean strace / stats harness | **Invalidated** — measured `start_lean()` not production layout |
| wip3/wip4d **zenoh slice** and "zenoh is free" narrative | **Invalidated** |
| wip4d production **7.48%** vs wip3 **8.64%** | **Invalidated** — both had broken zenoh |
| wip3 "production parity with wip2 via zenoh fix" | **Invalidated** |

### Fix (`2ae518a`, deployed 2026-07-06)

Dedicated **`zenoh-plane` OS thread** with `multi_thread` Tokio runtime in
`start_dual_plane()`:

- `runtime::spawn_zenoh()` for session open in `session.rs`
- Zenoh drivers spawned on `zenoh_handle` in `HubActor::add_driver`
- Data plane stays `current_thread`; control stays `current_thread` (wip4d)

Post-fix benchmark confirms zenoh working — see § **"wip4d post-fix (zenoh-plane)"**.

## Invalidated: --no-web lean tuning (2026-07-07)

**Discovery:** Lean benchmarks on **wip4c/wip4d** reported **~3% CPU** vs **~13%**
production and were used to claim the lean routing regression was "fixed." Those
numbers are **not comparable to production** because `--no-web` previously changed
far more than which drivers run.

### What `--no-web` used to do (pre-`20cc54c`, **invalid**)

| Behavior | Production | `--no-web` (old) |
|----------|------------|------------------|
| Web server `:8080` | on | off |
| Default REST/WebSocket driver | on | off |
| Zenoh drivers | on (from endpoints) | off (stripped from CLI **and** not filtered consistently) |
| Runtime | `start_dual_plane()` (3 OS threads) | **`start_lean()`** — single `current_thread` reactor |
| Clock | 1 ms cached refresher | **50 ms** cached refresher |
| Thread count | **12** (with zenoh) | **1** |

The **−4% lean CPU** attributed to wip4c (`7.33% → 3.24%`) was mostly
**`start_lean()` + 50 ms clock**, not routing fan-out. Strace lean tables
(`clock_gettime` ~520/s, `eventfd write` ~17/s) reflect that tuned layout, not
production with zenoh/web disabled.

### Fix (`20cc54c`, 2026-07-07)

`--no-web` now differs from production in **only** these ways:

1. No `:8080` web server (`main.rs` waits on `ctrl_c` instead of `web::run`).
2. No default REST/WebSocket driver (`cli::endpoints()`).
3. Zenoh/`zenohraw` drivers filtered out of the endpoint list (`cli::endpoints()`).

**Everything else is identical:** `start_dual_plane()` runtime (data CT +
control CT + zenoh MT plane), direct microsecond `now_micros()` reads, same
routing hot path.

### What remains valid vs invalidated

| Measurement | Status |
|-------------|--------|
| wip4d **production** post-`2ae518a` (**13.09%**, zenoh working) | **Valid** (re-benchmark under `20cc54c` pending) |
| wip4c/wip4d **lean** rows (`2.91%`, `3.24%`, strace ~520 `clock_gettime`/s) | **Invalidated** |
| "wip4d beats wip2 on lean" | **Invalidated** |
| wip3 lean regression diagnosis (cross-plane `eventfd` wakeups) | **Valid** as historical root-cause analysis |
| `start_lean()` / 50 ms lean clock as optimization targets | **Invalidated** — removed |

### Benchmark harness impact

- **`benchmark_lean_routing_endpoints`** still strips `zenoh:*` from the
  production cmdline for isolated cases. With `20cc54c`, prefer the **full
  production endpoint list + `--no-web`** so zenoh is disabled via
  `cli::endpoints()` filtering — same binary layout, only drivers differ.
- Lean vs production CPU comparisons must use the **same runtime**; do not treat
  `--no-web` as a license to change clocks or reactors.

## Invalidated: cached-clock shortcut (2026-07-07)

**Discovery:** Slowing the global clock refresher to **50 ms** on production
(reportedly **9.35% → 5.63%** CPU, `clock_gettime` 5607/s → 1849/s) looked like
a major win. It is **invalid** for two independent reasons:

1. **Measurement conditions:** production strace/pidstat during that run had **no
   WebSocket clients** connected (`web: NOT listening on :8080` in preflight for
   some runs; REST JSON transcode path idle). The saving did not reflect full
   production load.
2. **Product requirement:** delay and jitter stats need **microsecond** resolution
   (`now - message.timestamp` in `AtomicAccumulatedStats::update`). A cached
   clock stale by 50 ms (or even 1 ms) quantizes both sides and cannot measure
   **1 µs** latency/jitter.

### Fix (`20cc54c`)

Removed the background clock cache entirely. `now_micros()` reads
`SystemTime::now()` on every call. Cost is higher on musl (real `clock_gettime`
per call, roughly `4 + endpoints` times per frame) but timestamps and derived
stats are correct.

### Ruled out

- **Unified 50 ms refresher** for production or lean — breaks µs jitter; invalid
  benchmark conditions.
- **1 ms cached refresher** as a middle ground — still quantizes sub-ms jitter;
  only acceptable if product accepts **1 ms** granularity (it does not).

## Applied: sysinfo and ingress clock dedup (2026-07-07)

After **`20cc54c`** restored microsecond timestamps and aligned `--no-web` with
production, re-profiled on the Pi with **`dev/profile-prod-with-ws.sh`** (REST
WebSocket client on `ws://127.0.0.1:8080/v1/rest/ws` so the JSON broadcast path
is exercised). Settings: `WARMUP_SECS=10`, `MEASURE_SECS=10`, 12 threads, single
navigator. Binary md5 **`927f47a7`** (parent **`b382ed04`** = `20cc54c`).

### Baseline (`20cc54c`, `b382ed04`)

| Signal | No WS during strace | WS client active |
|--------|---------------------|------------------|
| CPU (n=30, pidstat) | **7.50% ±0.24%** | — |
| `clock_gettime64`/s | 3853 | 3736 |
| `open`/s | 505 (~228 ENOENT/s) | 353 |
| `getdents64`/s | 243 | 245 |
| Total syscalls/10s | 64167 | 62754 |

`open` + `getdents64` at **~600/s** traced to **`sysinfo`** in
`stats/resources.rs` (1 Hz resource refresh for the web UI). `clock_gettime` at
**~3800/s** is the cost of direct `now_micros()` on musl plus redundant back-to-back
reads at ingress (`Protocol::new` then `update_input` then hub message stats).

### Hypotheses tested

1. **Replace `sysinfo` with `/proc/self/stat` + `/proc/meminfo`** — same
   `ResourceUsage` fields, no directory walks.
2. **`note_ingress()`** — reuse `message.timestamp` for stats recorded immediately
   after `Protocol` construction; egress paths still call `now_micros()` for real
   transit delay.

### Results (`927f47a7`, both fixes, WS client during strace)

| Signal | Before | After | Δ |
|--------|--------|-------|---|
| CPU (n=30, WS during sample) | — | **6.48% ±0.14%** | — |
| CPU (n=30, no WS during sample) | 7.50% | **5.43% ±0.22%** | **−1.1% abs** |
| `clock_gettime64`/s (WS strace) | 3736 | **3330** | **−11%** |
| `open` + `getdents64`/s | ~600 | **0** (dropped from top-20) | **~−100%** |
| Total syscalls/10s | 62754 | **49654** | **−21%** |

### Decision

**Keep both changes.** Microsecond egress delay/jitter preserved; resource metrics
unchanged in meaning; syscall storm from `sysinfo` eliminated.

### Code

- `src/lib/stats/resources.rs` — read `/proc/self/stat`, `/proc/meminfo`,
  `/proc/uptime` directly; drop `sysinfo` dependency.
- `src/lib/stats/accumulated/` — `note_ingress()` / `note_input()` on driver
  ingress paths.
- `dev/profile-prod-with-ws.sh` — production strace/pidstat with WS client.

## wip4d full discrimination (2026-07-06)

### wip4d post-fix (zenoh-plane, `2ae518a`)

**Commit:** `2ae518a` — dedicated `zenoh-plane` OS thread with `multi_thread`
runtime; zenoh drivers and session on `zenoh_handle`. Builds on `a16b3e3`
(control `current_thread`). **md5:** `0b4b4809`.

**Harness:** `dev/run-bench-wip4d.sh`, n=75/case. Log:
`/tmp/bench-wip4d-zenoh-fix.log` (inside `blueos-core`).

#### Stats harness results (mean ± ci95, n=75)

| Config | **wip2** (`95e42435`) | **wip3** (`6841d042`)† | **wip4d post-fix** (`0b4b4809`) |
|--------|----------------------|------------------------|----------------------------------|
| full production | **8.54% ±0.21%** | **8.64% ±0.17%**† | **13.09% ±0.26%** |
| no zenoh | **4.99% ±0.09%** | **8.29% ±0.16%** | **7.07% ±0.13%** |
| lean (`--no-web`) | **4.27% ±0.11%** | **7.33% ±0.14%** | **2.91% ±0.09%**‡ |
| lean head-to-head | **4.49% ±0.12%** | **7.33% ±0.15%** | **3.09% ±0.09%** |
| `mavlink-routerd` v4 | **1.95% ±0.08%** | **1.93% ±0.10%** | **1.99% ±0.08%** |
| Prod OS threads | ~5 | ~6† | **12** |

†wip3 production/zenoh rows measured with zenoh **broken** (data-plane
`current_thread`) — not comparable to post-fix wip4d.
‡wip4d lean row **invalidated** post-`20cc54c` — measured `start_lean()` layout.

#### Incremental decomposition (post-fix)

**wip4d production (13.09%)**

```
mavlink-routerd reference           ~1.79%
  + routing fan-out (no web)        +1.12%   →  lean = 2.91%
  + web/REST                        +4.16%   →  no-zenoh = 7.07%
  + zenoh + zenohraw                 +6.02%   →  production = 13.09%
```

| Component | ~CPU% | Share of production |
|-----------|-------|---------------------|
| Routerd baseline | ~1.8% | 14% |
| Routing core (lean − routerd) | ~1.1% | 8% |
| Web/REST | ~4.2% | 32% |
| **Zenoh stack** | **~6.0%** | **46%** |
| **Total** | **13.1%** | 100% |

Lean vs `mavlink-routerd`: **3.09% vs 1.99%** → **+1.10%** (+1.6×).

#### vs pre-fix broken-zenoh run (`a2cdd520`, same harness)

| Config | Pre-fix | Post-fix | Δ |
|--------|---------|----------|---|
| Production | 7.48% | **13.09%** | **+5.61%** |
| No zenoh | 7.46% | **7.07%** | −0.39% |
| Zenoh slice | ~0% | **+6.02%** | zenoh restored |
| Lean | 3.24% | **2.91%** | −0.33% (noise) |
| Threads | 2 | **12** | zenoh workers present |

#### Interpretation

- **Lean wins are real** — post-fix lean **2.91%** beats wip2 **4.27%** and wip3
  **7.33%** (significant; ci95 2.82–3.00% vs 4.16–4.38%).
- **Production at 13.09% exceeds wip2 8.54%** — dual-plane with three OS threads
  (data CT + control CT + zenoh MT) plus working zenoh costs more than wip2's
  single `multi_thread` layout when zenoh is active.
- **wip2/wip3 headline production numbers understated true cost** — wip3 zenoh
  slice was ~0% because zenoh was not running; extrapolating wip3 with working
  zenoh would land near **~14%** (8.29% no-zenoh + ~6% zenoh), similar to wip4d.
- **Zenoh slice +6.0% vs wip2 +3.55%** — higher on wip4d; likely extra cross-runtime
  coordination (zenoh-plane vs unified runtime) plus measurement variance.

---

### wip4d pre-fix (broken zenoh, `a2cdd520`)

> **Invalid for production/zenoh.** Measured before `2ae518a`. Zenoh not running.
> **Lean rows remain valid** as reference for wip4c fixes.

**Goal:** isolate what **wip4d** (`a16b3e3`, md5 `a2cdd520`) contributes on top
of the wip3 → wip4c line, with the same stats harness used for wip2/wip3.

### Ancestry and code delta

```
wip3 (667336a)  PlaneRuntimes dual-plane; lean regressed; zenoh broken on data CT
  └─ wip4c (fb6f8a9)  start_lean() single reactor; 50 ms lean clock; direct register_sink
       └─ wip4d (a16b3e3)  production control plane: multi_thread → current_thread
            └─ 2ae518a  zenoh-plane OS thread (multi_thread); zenoh drivers on zenoh_handle
```

| Commit | Files | Functional change |
|--------|-------|-------------------|
| wip4c | `runtime.rs`, `time.rs`, `generic_tasks.rs` | Lean: 1-thread reactor; slower lean clock; bypass hub `RegisterSink` |
| wip4d | `runtime.rs` (3 lines) | Production: control `Handle` on `current_thread` main instead of 4-worker pool |
| **2ae518a** | `runtime.rs`, `session.rs`, `hub/actor.rs`, `drivers/mod.rs` | Zenoh: dedicated `multi_thread` plane; drivers + session on `zenoh_handle` |

Lean `--no-web` path is **identical** between wip4c and wip4d (`start_lean()`).
All lean deltas below are **wip4c fixes vs wip3**; production deltas are **wip4d
vs wip3** (wip4c production layout matched wip3: 6 threads).

### Harness

`dev/run-bench-wip4d.sh` (codec-impact + lean-router, one `flock` lock).
Settings: `BENCH_TRIALS=5`, `BENCH_SAMPLES=15`, `BENCH_INTERVAL_SECS=2`,
`WARMUP_SECS=10` → **n=75** pidstat samples per case. Companion scripts:
`dev/profile-lean-strace.sh` (lean syscalls), `dev/profile-prod-strace.sh`
(production syscalls on manager-spawned PID).

Log: `/tmp/bench-wip4d-full.log` (pre-fix), `/tmp/bench-wip4d-zenoh-fix.log`
(post-fix), `/tmp/profile-wip4d-prod-strace.log`, `/tmp/profile-wip4d-strace.log`
(inside `blueos-core`).

### Stats harness results — pre-fix only (mean ± ci95, n=75)

| Config | **wip3** (`6841d042`) | **wip4d** (`a2cdd520`) | Δ (wip4d − wip3) |
|--------|----------------------|------------------------|------------------|
| full production (routing + zenoh + web/REST) | **8.64% ±0.17%** | **7.48% ±0.13%** | **−1.16%** |
| no zenoh (routing + web/REST) | **8.29% ±0.16%** | **7.46% ±0.18%** | **−0.83%** |
| lean (routing only, `--no-web`) | **7.33% ±0.14%** | **3.24% ±0.12%** | **−4.09%** |
| `mavlink-routerd` v4 (codec-impact) | **1.93% ±0.09%** | **1.97% ±0.08%** | +0.04% |
| lean head-to-head (`benchmark-lean-router.sh`) | **7.33% ±0.15%** | **3.18% ±0.11%** | **−4.15%** |
| `mavlink-routerd` v4 (lean-router) | **1.93% ±0.10%** | **1.98% ±0.07%** | +0.05% |

`mavlink-routerd` stable across builds — deltas are all in mavlink-server.

### Statistical significance (ci95 intervals)

| Comparison | wip3 interval | wip4d interval | Significant? |
|------------|---------------|----------------|--------------|
| Production | 8.47–8.81% | 7.35–7.61% | **Yes** — wip4d lower |
| No zenoh | 8.13–8.45% | 7.28–7.64% | **Yes** — wip4d lower |
| True lean (`--no-web`) | 7.19–7.47% | 3.12–3.36% | **Yes** — wip4d lower |
| Zenoh slice (prod − no-zenoh) | +0.19–0.51% | −0.16–0.20% | **Invalid** — zenoh broken |
| Lean vs wip2 lean (4.16–4.38%) | — | 3.12–3.36% | **Yes** — wip4d lower than wip2 |
| Lean vs wip2 lean | wip2 wins? | wip4d wins | wip4d **beats wip2** on lean |

### Incremental decomposition

**wip4d production (7.48%)**

```
mavlink-routerd reference           ~1.98%
  + routing fan-out (no web)        +1.26%   →  lean = 3.24%
  + web/REST                        +4.22%   →  no-zenoh = 7.46%
  + zenoh + zenohraw                 +0.02%†   →  production = 7.48%
```

†Zenoh slice **invalid** — zenoh not running on this build.

| Component | ~CPU% | Share of production |
|-----------|-------|---------------------|
| Routerd baseline | ~2.0% | 27% |
| Routing core (lean − routerd) | ~1.3% | 17% |
| Web/REST | ~4.2% | 56% |
| **Zenoh stack** | **~0.0%†** | **0%** |
| **Total** | **7.5%** | 100% |

Lean vs `mavlink-routerd`: **3.18% vs 1.98%** → **+1.20%** (+1.6×). Compare
wip3: **7.33% vs 1.93%** → **+5.40%** (+3.8×).

**Discriminating the two commit layers:**

| Layer | What changed | Lean CPU | Production CPU | Prod threads |
|-------|--------------|----------|----------------|--------------|
| wip3 | dual-plane baseline | 7.33% | 8.64% | 6 |
| **+ wip4c** (lean fixes) | `start_lean`, 50 ms clock, direct sink | **3.24%** | *(same 6-thread prod layout; not re-measured)* | 1 lean / 6 prod |
| **+ wip4d** (prod control CT) | control `current_thread` on main | 3.24% (unchanged) | **7.48%** | **2** |

- **wip4c layer** accounts for **−4.1% lean** (7.33 → 3.24) — the wip3 lean
  regression. Strace shows coordination syscalls (`clock_gettime`, `eventfd write`)
  drop to wip2 levels (§ "Lean routing strace comparison").
- **wip4d layer** accounts for **−1.2% production** (8.64 → 7.48) and **−67%
  threads** (6 → 2) with lean unchanged. Removing the 4-worker control pool
  eliminates cross-reactor `futex`/worker wakeups without touching the data
  plane or zenoh drivers.

### Thread model

| Mode | wip3 | wip4d |
|------|------|-------|
| Lean (`--no-web`) | 6 (data CT + control MT) | **1** (`start_lean`) |
| Production | 6 (data CT OS thread + control MT 4 workers) | **2** (data CT OS thread + control CT main) |

Production strace attached with **2 threads** (`data-plane` + main).

### Syscall attribution

**Lean** (`profile-lean-strace.sh`, 10 s strace, isolated bench):

| Syscall /s | wip3 | wip4d |
|------------|------|-------|
| `sendto` | 299 | 300 |
| `recvfrom` | ~110 | 114 |
| `clock_gettime64` | 2732 | **509** |
| `write` (total) | 350 | **16** |
| `eventfd` (`fd=4`) | ~350 | **~16** |
| `futex` | 0 | 0 |

**Production** (`profile-prod-strace.sh`, 10 s strace, manager PID):

| Syscall /s | wip4d prod | Notes |
|------------|------------|-------|
| `sendto` | 299 | Same fan-out as lean |
| `recvfrom` | 115 | Same |
| `clock_gettime64` | **4006** | Both planes use 1 ms refresher |
| `write` | **551** | zenoh + web + dual-reactor wakeups |
| `epoll_pwait` | 618 | Two reactors, two epoll fds |
| CPU (pidstat window) | ~7–8% | Pre-fix broken zenoh; post-fix harness **~13%** |

Production `clock_gettime` stays high (4006/s) because the 50 ms lean-only clock
slowdown does not apply to production. Pre-fix production strace (~7–8%) reflected
**broken zenoh**; post-fix production is **13.09%** (stats harness).

### Summary (post-fix)

| Question | Answer |
|----------|--------|
| Does wip4d fix lean routing? | **Invalidated** — lean used `start_lean()`; re-benchmark under `20cc54c` |
| Is zenoh working? | **Yes** — +6.02% slice, 12 threads |
| True production CPU? | **13.09% ±0.26%** (not the pre-fix 7.48%) |
| vs wip2 production (8.54%)? | **Higher** — wip2 also had working zenoh but single runtime; dual-plane + zenoh-plane costs more |
| vs wip3 headline (8.64%)? | **Not comparable** — wip3 benchmark had broken zenoh |
| Residual lean gap vs routerd? | **+1.1%** (3.09% vs 1.99%) |

### Summary (pre-fix, historical)

Pre-fix run (`a2cdd520`): production **7.48%**, zenoh **broken**, 2 threads.
Superseded by post-fix section above. Lean **3.24%** remains a valid wip4c
reference.

## Why wip3 regressed lean routing

Measured with `dev/profile-lean-strace.sh` and `dev/profile-lean-writes.sh`
(same isolation as `benchmark-lean-router.sh`: pause manager with `SIGSTOP`, kill
watchdog, run fresh `/tmp/mavlink-server-bench --no-web` copy). strace attached
for 10 s after 10 s warmup; rates below are per-second averages over the strace
window.

**The regression is not packet handling.** `sendto` (~300/s) and `recvfrom`
(~110/s) are identical across wip2, wip3, wip4, wip4b, wip4c, and wip4d — the
same
MAVLink fan-out work reaches the kernel. User-visible routing logic (decode,
broadcast, re-encode per endpoint) is unchanged in cost at this message rate.

**The extra CPU is syscall-dominated runtime coordination**, almost all in
`sys` time on wip3 (pidstat shows ~0% usr, ~8% sys vs wip2's ~0% usr, ~4% sys).

### wip3 (`667336a`): data `current_thread` + control `multi_thread`

| Mechanism | wip2 | wip3 | Δ |
|-----------|------|------|---|
| OS threads (lean) | 5 | 6 | +1 (`data-plane` thread) |
| `clock_gettime64`/s | 2216 | 2732 | +516 |
| `write` → `eventfd`/s | 37 | 350 | **+313** |
| `futex`/s | 231 | 0 | moved to eventfd path |
| `epoll_pwait`/s | 105 | 388 | +283 |

`dev/profile-lean-writes.sh` on wip3 shows the dominant `write` target:

```
fd=4  anon_inode:[eventfd]  ~518 writes/s
sample: write(4, "\1\0\0\0\0\0\0\0", 8)   # tokio Notify wakeup
```

On wip3 the data plane runs a dedicated `current_thread` runtime on a
`data-plane` OS thread (`src/lib/runtime.rs`). Every I/O readiness edge, timer
tick (including the 1 ms `time.rs` refresher), and cross-plane task spawn must
**explicitly wake that reactor** via an `eventfd` write. wip2's single
`multi_thread` runtime amortizes the same wakeups internally (~37 eventfd
writes/s).

The wip3 data-path refactor (DataPlane, `register_sink` via hub actor,
`SyncMessageFilters`, loopback filtering in `SinkReceiver::recv`) adds structure
but is not the dominant term — the syscall delta above accounts for the measured
**+3% sys CPU** (7.33% vs 4.27% stats harness).

### wip4 (`ab20b48`): unified `multi_thread` when `--no-web`

Collapses data and control onto one `multi_thread` runtime for lean mode
(`PlaneRuntimes::start_unified`). Thread count returns to **5**, but lean CPU
measured **worse than wip3** (~10% pidstat) in strace runs:

| Mechanism | wip2 | wip3 | wip4 |
|-----------|------|------|------|
| Threads | 5 | 6 | 5 |
| CPU (lean, pidstat) | ~4% | ~7.7% | **~10%** |
| `clock_gettime64`/s | 2216 | 2732 | **4144** |
| `write` → `eventfd`/s | 37 | 350 | **353** |
| `futex`/s | 231 | 0 | **350** |

Unified runtime removes the cross-thread data-plane split but **does not remove
the wip3 data-path overhead**. `clock_gettime64` actually rises further (two
competing theories: strace run-to-run variance vs more per-message timestamp
reads through the DataPlane/stats path). usr CPU appears (~3%) where wip2/wip3
lean had ~0%.

### wip4b (`ad6b56b`): dual `current_thread`

Both planes use `current_thread` — data on a `data-plane` OS thread, control on
the main thread via `block_on`. No worker pool on either side → **2 threads**,
**0 futex** in strace.

| Mechanism | wip2 | wip3 | wip4 | wip4b |
|-----------|------|------|------|-------|
| Threads | 5 | 6 | 5 | **2** |
| CPU (lean, pidstat) | ~4% | ~7.7% | ~10% | **~6.8%** |
| usr / sys | ~0% / ~4% | ~0% / ~8% | ~3% / ~8% | **~1.7% / ~5%** |
| `clock_gettime64`/s | 2216 | 2732 | 4144 | **4701** |
| `write` → `eventfd`/s | 37 | 350 | 353 | **587** |
| `epoll_pwait`/s | 105 | 388 | 362 | **769** |
| `futex`/s | 231 | 0 | 350 | **0** |
| `sendto`/s | 299 | 299 | 288 | 300 |

wip4b is the **best failed dual-reactor variant** (~6.8% vs wip3's 7.7% and
wip4's ~10%): eliminating worker pools removes futex contention. But **two
`current_thread` reactors means two `eventfd`s** (fd=4 data, fd=11 control in
writes trace), doubled `epoll_pwait`, and the **highest `clock_gettime64` rate
among the regressions** (4701/s — each runtime runs its own 1 ms refresher).

### wip4c (`fb6f8a9`): single `current_thread` lean + hot-path fixes

> **Invalidated post-`20cc54c`.** The changes below were applied only on the
> `--no-web` path (`start_lean()`, 50 ms clock). That path no longer exists;
> see § "Invalidated: --no-web lean tuning".

Three changes in one commit fix lean routing while keeping dual-plane production:

1. **`PlaneRuntimes::start_lean()`** — when `--no-web`, one `current_thread`
   runtime; data and control share the same `Handle` ( **1 OS thread** ).
2. **Clock refresher slowed to 50 ms on lean** (`time.rs`; production stays
   1 ms) — cuts timer wakeups on the single reactor.
3. **Direct `data_plane.register_sink_with_origin()`** on the egress hot path
   (`generic_tasks.rs`) — bypasses the async hub `RegisterSink` round-trip.

| Mechanism | wip2 | wip3 | wip4b | **wip4c** |
|-----------|------|------|-------|-----------|
| Threads | 5 | 6 | 2 | **1** |
| CPU (lean, pidstat) | ~4% | ~7.7% | ~6.8% | **~3.0%** |
| usr / sys | ~0% / ~4% | ~0% / ~8% | ~1.7% / ~5% | **~0.7% / ~2.5%** |
| `clock_gettime64`/s | 2216 | 2732 | 4701 | **520** |
| `write` → `eventfd`/s | 37 | 350 | 587 | **17** |
| `epoll_pwait`/s | 105 | 388 | 769 | **73** |
| `futex`/s | 231 | 0 | 0 | **0** |
| `sendto`/s | 299 | 299 | 300 | **299** |
| Total syscalls/s | 3292 | 4177 | 6766 | **1328** |

`dev/profile-lean-writes.sh` on wip4c: `fd=4 eventfd` **~15 writes/s** (lowest
measured). Coordination syscall rates are **at or below wip2** on every metric.

**Paradox resolved:** `current_thread` is cheaper than `multi_thread` *only with
a single reactor*. wip2's 4-worker `multi_thread` was the cheapest *failed*
wip3+ topology because it batches wakeups internally; wip4c beats both by
collapsing to one reactor and removing hub/timer overhead on the lean path.

### wip4d (`a16b3e3`): `current_thread` production control plane

One commit on top of wip4c: production `start_dual_plane()` control plane
switches from `multi_thread` (4 workers) to `current_thread` on main. Lean path
unchanged (`start_lean()`).

Full stats-harness discrimination (production, no-zenoh, lean, routerd,
significance, decomposition, production strace) is in § **"wip4d full
discrimination"**. Strace lean snapshot vs wip4c:

| Mechanism | wip4c | **wip4d** |
|-----------|-------|-----------|
| Threads (lean) | 1 | **1** |
| CPU (lean, pidstat) | ~3.0% | **~3.2%** |
| `clock_gettime64`/s | 520 | **509** |
| `write` → `eventfd`/s | 17 | **16** |
| `sendto`/s | 299 | **300** |

## Lean routing strace comparison (2026-07-06)

Harness: `dev/profile-lean-strace.sh` (sources `benchmark-common.sh`, adds
`strace -c -f` + `pidstat -u/-w`). Companion `dev/profile-lean-writes.sh`
breaks down `write` syscalls by fd. Settings: `WARMUP_SECS=10`,
`MEASURE_SECS=10`, single trial. Logs: `/tmp/profile-wip4-suite.log`,
`/tmp/profile-wip4b-suite.log`, `/tmp/profile-wip4c-suite.log`,
`/tmp/profile-wip4d-strace.log` (inside `blueos-core`).

### Summary table

| Build | Commit | md5 | Threads | CPU (lean) | `clock_gettime`/s | `eventfd write`/s | `futex`/s | `sendto`/s |
|-------|--------|-----|---------|------------|-------------------|-------------------|-----------|------------|
| wip2 | `50ffb99` | `95e42435` | 5 | ~4% | 2216 | 37 | 231 | 299 |
| wip3 | `667336a` | `6841d042` | 6 | 7.7% | 2732 | 350 | 0 | 299 |
| wip4 | `ab20b48` | `d691a68d` | 5 | ~10% | 4144 | 353 | 350 | 288 |
| wip4b | `ad6b56b` | `210c1fc1` | 2 | ~6.8% | 4701 | 587 | 0 | 300 |
| **wip4c** | **`fb6f8a9`** | **`51eb8734`** | **1**‡ | **~3.0%**‡ | **520**‡ | **17**‡ | **0** | **299** |
| **wip4d** | **`a16b3e3`** | **`a2cdd520`** | **1**‡ | **~3.2%**‡ | **509**‡ | **16**‡ | **0** | **300** |

‡**Invalidated** — `start_lean()` / 50 ms lean clock; not production layout.

CPU figures from pidstat during strace window (not the n=75 stats harness — use
that for production/significance; use strace runs for syscall attribution).
wip2/wip3 lean CPU from the 2026-07-05 stats harness; wip3/wip4/wip4b/wip4c/wip4d
strace from 2026-07-06 on the same Pi with a single live navigator. wip4d
production CPU from stats harness on 2026-07-06 (see wip4d subsection in §
"Why wip3 regressed lean routing").

### Runtime topology by branch

| Branch | Lean runtime layout (historical) | Post-`20cc54c` |
|--------|----------------------------------|----------------|
| wip2 | Single `multi_thread` (`#[tokio::main]`, 4 workers) | — |
| wip3 | `data-plane` OS thread: `current_thread`; control: `multi_thread` (4 workers) | — |
| wip4 | `--no-web`: unified `multi_thread`; else same as wip3 | — |
| wip4b | `data-plane` OS thread: `current_thread`; control: `current_thread` on main | — |
| wip4c | `--no-web`: single `current_thread` (`start_lean`); production: data CT + control MT | **Removed** |
| wip4d | `--no-web`: same as wip4c; production: data CT + control CT + zenoh MT | **`--no-web` = production minus web/REST/zenoh only** |

### Profiling workflow

```sh
# On host: build + deploy
./cross_build_and_run.sh

# In container: copy strace from host once, then profile
docker cp /usr/bin/strace blueos-core:/tmp/strace   # from Pi host
cd /tmp/dev
BENCH_TRIALS=1 BENCH_SAMPLES=4 WARMUP_SECS=10 MEASURE_SECS=10 \
  STRACE_BIN=/tmp/strace bash ./profile-lean-strace.sh
MEASURE_SECS=5 STRACE_BIN=/tmp/strace bash ./profile-lean-writes.sh
```

Run **one suite at a time** under `flock` (`benchmark_session_begin` acquires
`/tmp/benchmark.lock`). Do not manually restart `ardupilot_manager` between runs —
let `benchmark_restore_managed` handle it. If duplicates accumulate, use
`benchmark_force_consolidate` from a sourced `benchmark-common.sh`.

### Harness lessons (2026-07-05)

- **`benchmark_restore_managed` must kill `ardupilot_navigator` children** before
  restarting the manager; otherwise each benchmark cycle orphans a navigator and
  duplicates MAVLink traffic (invalidates CPU numbers and can exhaust RAM).
- **Cache production cmdline before isolation** (`benchmark_cache_production_cmdline`);
  re-reading endpoints after killing the managed instance yields empty args.
- **Build `LEAN_ARGS` after `benchmark_prepare_bench_binary`** — a stale
  `/tmp/mavlink-server-bench` from a prior wip3 run caused false `--no-web`
  detection on 0.9.0.
- **`--no-web` probe uses `timeout 2 "$bin" --no-web`** — piping to `grep` hangs
  when the flag is accepted (wip3) and returns a false positive.
- **`benchmark_prepare_bench_binary` logs to stderr** — stdout is consumed by
  `mapfile` for endpoint args.
- With **`20cc54c`**, prefer **production endpoints + `--no-web`** for lean cases
  instead of stripping `zenoh:*` in shell — zenoh is filtered in `cli::endpoints()`.
- Tighten PID matching to actual binary paths (`/usr/bin/mavlink-server`), not
  shell cmdlines that mention the path.
- Export `BENCH_TRIALS` / `BENCH_SAMPLES` **before** sourcing
  `benchmark-common.sh` in suite runners.
- **Do not manually restart `ardupilot_manager` via tmux** between benchmark
  runs — use `benchmark_restore_managed` or `benchmark_force_consolidate`.
  Wrong `run-service` quoting or repeated `C-c`/`run-service` cycles spawn
  duplicate managers and mavlink-server instances, invalidating CPU numbers.
  Correct watchdog command (from `benchmark_start_autopilot_supervision`):
  `run-service 'autopilot' 'nice --19 /home/pi/services/ardupilot_manager/main.py' 0 0 0 0`
- **`cross_build_and_run.sh` sends `C-c` to the autopilot tmux session** after
  deploy; run `benchmark_force_consolidate` or the watchdog command above before
  profiling if the manager was left stopped.
- Long profiling suites should run **inside the container** (`docker exec -d
  blueos-core bash -lc '…'`) so an SSH disconnect does not kill mid-restore.
  The `flock` lock must still be held for the full suite; do not start a second
  suite while one holds `/tmp/benchmark.lock`.

## Zenoh deep-dive (2026-07-06): tracing, publisher cache, SHM, per-field fan-out

After restoring working zenoh on wip4d (`2ae518a`), production sits at **13.09%**
with zenoh the largest slice (**+6.02%**). This section drills into *why* zenoh is
expensive, using `perf record` on a symbolized `profiling` binary (see § below)
plus A/B measurements. Two fixes were applied and committed; two hypotheses were
measured and ruled out; the per-field fan-out was quantified directly.

### Profiling build

`[profile.profiling]` in `Cargo.toml` now inherits `release` but keeps symbols so
`perf` can unwind and symbolize user space:

```toml
[profile.profiling]
inherits = "release"
debug = 2
strip = false
force-frame-pointers = true
```

Baseline symbolized `perf record -g -F 999` on the manager-spawned production
process: ~50% kernel / ~50% user, with zenoh's `zenoh-runtime` and `tx-0` threads
plus `tracing_subscriber` bookkeeping and `__libc_malloc_impl` as the top
user-space consumers. The `zenohd` **router** is a *separate* process — it does
not appear in mavlink-server's profile but consumes **~9–10% of one core** on its
own (see per-field table below).

### Applied fix 1 — tracing filter (committed)

`src/lib/logger.rs` defaulted the file and server log layers to
`LevelFilter::DEBUG` when not tracing. `tracing_subscriber` therefore evaluated
and built every dependency `DEBUG`/`TRACE` span (zenoh, tokio, hyper…) before
discarding the output — pure bookkeeping cost. Changed both layers to
`EnvFilter::new("info,mavlink_server=debug")`.

| Metric | Before | After |
|--------|--------|-------|
| `tracing_subscriber` self-time (perf) | ~6% on-CPU | ~2% on-CPU |
| Absolute CPU | — | **~0.5% saved** |

### Applied fix 2 — zenoh publisher cache (committed)

`src/lib/drivers/zenoh/json.rs` rebuilt topic strings with `format!` and did a
`HashMap` lookup **per message and per field** on every packet — thousands of
`String` allocations/s. Replaced with a `HashMap<(u8,u8,u32), MessagePublishers>`
cache keyed by `(system_id, component_id, message_id)`, holding the declared
`Publisher` for the message and each field.

| Metric | Before | After |
|--------|--------|-------|
| `__libc_malloc_impl` self-time (perf) | 0.91% on-CPU | 0.35% on-CPU |
| Absolute CPU | — | **~0.1–0.15% saved** |

### Ruled out — zenoh HLC timestamping

Hypothesis: zenoh's Hybrid Logical Clock timestamping drove the high
`clock_gettime` rate. A/B by explicitly setting
`timestamping/enabled = {router,peer,client}=false` in `session.rs` showed **no
change** in `clock_gettime` rate or CPU. Timestamping is **already off by default
in client mode**. Change reverted. With **`20cc54c`**, `clock_gettime` rate
comes from direct `now_micros()` reads (~`4 + endpoints` per frame on musl), not
a background refresher.

### Ruled out — shared memory would make zenoh "free"

Zenoh 1.9.0 defaults (`zenoh-config-1.9.0/src/defaults.rs`):

```
ShmConf { enabled: true }
LargeMessageTransportOpt { enabled: true, message_size_threshold: 3072 }
```

Both **enabled by default**. But SHM's transport optimization only engages for
payloads **≥ 3072 bytes**. Measured real MAVLink JSON payload sizes (Python
`mavlink/**` subscriber, 5000 samples):

| Topic class | samples | mean bytes | max bytes |
|-------------|---------|-----------|-----------|
| `mavlink/out` + per-message | ~1080 | ~288 | 1899 |
| per-field | ~3920 | ~12 | small |
| **overall** | 5000 | **71.3** | **1899** |

**0 of 5000** payloads exceeded 3072 bytes → SHM never activates; everything goes
over TCP loopback. This is **by design** — copying sub-3 KB buffers is cheaper
than SHM ring-buffer bookkeeping. So the zenoh cost is **not** byte copying that
SHM could remove; it is **per-operation overhead × operation count** plus the
**two-hop client→router→client** routing path.

### Measured — per-field fan-out share (A/B, 2026-07-06)

`~108 MAVLink msgs/s` expand to `~1000 zenoh puts/s`: `mavlink/out` (108/s) +
per-message topics (108/s) + **per-field topics (784/s, 78% of all puts)**.

A/B on the **same** binary via an env toggle (`ZENOH_PUBLISH_FIELDS=0` skips
declaring/publishing field topics — measurement-only, not committed), 4
interleaved ON/OFF reps, 20 s windows, with a `mavlink/**` recording subscriber
present so every put is actually routed (matches production), manager paused via
`SIGSTOP`. CPU is `%` of one core from `/proc/<pid>/stat` deltas.

| | puts/s | mavlink-server | zenohd (router) | combined |
|---|---|---|---|---|
| **ON** (msg + fields) | ~1000 (784 field) | **10.89%** | **10.37%** | **21.26%** |
| **OFF** (msg only) | ~216 (0 field) | **10.36%** | **9.15%** | **19.51%** |
| **Δ (fields)** | −784 | **−0.52%** (noisy, ~2σ) | **−1.22%** (clean) | **−1.74%** |

Per-rep (mavlink-server / zenohd, %/core):

```
rep1  ON 10.70/10.60  OFF 10.35/9.20
rep2  ON 11.00/10.60  OFF 10.05/9.00
rep3  ON 10.55/9.80   OFF 10.60/9.25
rep4  ON 11.29/10.49  OFF 10.45/9.15
```

**Key finding: the per-field fan-out is *not* the main lever.** Dropping 78% of
all zenoh put operations saves only **~1.7% of one core combined** (~0.4% of the
4-core machine). Each field put costs ~22 µs-core across the full
publish→route→deliver path — tiny payloads are genuinely cheap. Even with fields
off and only 216 puts/s, mavlink-server still sits at **~10.4%/core** and zenohd
at **~9.2%/core**. The dominant cost is the **base message-level publishing**
(`mavlink/out` + per-message at 108 Hz) plus the underlying MAVLink
routing/serialization and the **router hop** — not the field multiplication.

> Measurement noise is large: a single 15 s sample swung between −2.3% and ~0%
> combined across the first two passes. Only the 4-rep interleaved average is
> trustworthy, and even then only the zenohd delta is statistically clean.

Scripts (in `dev/`, not deployed to production): `zmeasure.py` (subscriber +
`/proc` CPU sampler), `bench_fanout.sh` (manager-pause + interleaved ON/OFF
driver), `zcount.py` (payload/put-rate counter).

### Measured — whole zenoh driver on/off (A/B, 2026-07-06)

To split "how much of mavlink-server is zenoh vs the raw MAVLink routing that
`mavlink-routerd` also does," ran the **same** bench binary (with the two
committed fixes) under three endpoint sets, everything else identical (web/REST
on, full UDP/TCP fan-out), 3 interleaved reps, 20 s windows, manager paused:

- **FULL** — `zenoh:` (JSON driver) + `zenohraw:` (raw driver)
- **NOJSON** — `zenohraw:` only (JSON driver removed)
- **NOZENOH** — neither zenoh driver

| Config | mavlink-server (%/core) | zenohd (%/core) |
|--------|-------------------------|-----------------|
| FULL | **11.08** | **10.90** |
| NOJSON (raw only) | **9.35** | **8.13** |
| NOZENOH | **7.18** | **6.35** |

Deltas (mean of 3 reps):

| Component | mavlink-server | zenohd | **combined** |
|-----------|----------------|--------|--------------|
| **JSON driver** (FULL − NOJSON) | **+1.73** | **+2.77** | **+4.50** |
| **raw driver** (NOJSON − NOZENOH) | **+2.17** | **+1.78** | **+3.95** |
| **all zenoh** (FULL − NOZENOH) | **+3.90** | **+4.55** | **+8.45** |

Per-rep (mavlink-server / zenohd, %/core):

```
       FULL          NOJSON        NOZENOH
rep1   11.05/10.75   9.35/7.90     7.10/6.25
rep2   11.20/10.80   9.45/8.55     7.25/6.55
rep3   11.00/11.15   9.25/7.95     7.20/6.25
```

**Findings:**

- **Zenoh is ~35% of mavlink-server's own CPU** (3.90 of 11.08 %/core). The
  remaining **~7.2%/core** is raw MAVLink routing + web/REST + async runtime — and
  this floor matches the stats-harness "no zenoh" figure (7.07%) closely.
- **The true system cost of zenoh is ~8.5%/core**, because the separate `zenohd`
  router pays **~4.55%/core** on top of mavlink-server's **~3.90%/core**. The
  earlier stats harness only counted mavlink-server's slice.
- **The raw driver (`zenohraw`) costs as much as the JSON driver** — more on
  mavlink-server (2.17 vs 1.73 %/core), less on the router (1.78 vs 2.77). It is
  *not* free: it publishes/receives raw MAVLink frames over zenoh outside the
  `mavlink/**` JSON namespace (my subscriber saw `puts=0` for it), so it had been
  overlooked. Removing it saves ~3.95%/core combined.
- **This bench binary (with the tracing + publisher-cache fixes) shows FULL
  mavlink-server at ~11.1%/core vs the pre-fix stats-harness 13.09%** — consistent
  with the ~0.6–2% the two committed fixes were expected to shave.

**Consolidated zenoh cost model** (per-core, this binary, with recording
subscriber present):

```
mavlink-server FULL           ~11.1%   zenohd FULL     ~10.9%
  − raw driver     −2.2%                  − raw   −1.8%
  − JSON driver    −1.7%                  − JSON  −2.8%
  = non-zenoh floor ~7.2%                = base  ~6.4% (other BlueOS clients)
```

**All of this traffic is mandatory** — the JSON driver, the raw driver, and every
message/field topic are required production features (a router plus a recording
client always subscribe to all topics). So these numbers are the *price of the
required features*, not things to remove. The optimization target is to make the
**same** ~1000 puts/s cheaper per operation and per hop (batching flushes,
cheaper serialization, lower client↔router cost), not to publish less. Per-field
topics are a minor sub-component (~1.7%/core of the 4.5) — the base message-level
publishing + router hop dominate.

## Cost breakdown of the ~13% total (pre-codec)

| Component                                   | ~CPU% | Notes |
|---------------------------------------------|-------|-------|
| zenoh **JSON** driver                       | ~6.6  | JSON serialization of every message + zenoh runtime (7 threads, multicast) |
| base: RX + tokio runtime + web server :8080 | ~3.9  | floor with no outputs |
| TCP loopback link (`tcps:5777`)             | ~2.4  | full TCP transmit + loopback softirq RX per small message |
| 5 UDP outputs                               | ~1.6  | ~0.3% each — cheap |
| zenoh raw driver                            | ~1.5  | shares the zenoh runtime |

## Why mavlink-server > mavlink-router for the same work

1. **Zenoh JSON publishing** (~6.6%): per-message JSON serialization through a
   multi-threaded zenoh runtime. `mavlink-routerd` has no equivalent.
2. **TCP for an internal loopback link** (~2.4%): loopback TCP is ~8× a UDP
   output per message.
3. **Extra always-on machinery**: web server, second (raw) zenoh driver,
   multi-threaded runtime coordination (~1,300 ctx-sw/s).
4. **musl (no VDSO)**: `clock_gettime` becomes a real syscall ~3,400×/s;
   secondary but pure overhead the glibc `mavlink-routerd` avoids.

Even with all zenoh removed, mavlink-server is still heavier than mavlink-router
(see the head-to-head tables below). With the default web/REST stack still
running the gap is ~2.7×; stripping web/REST as well (`--no-web`) closes it to
~1.8–2.0× — so the base async routing architecture — not just zenoh — carries
real overhead, but ~1.5–2.5% of the earlier no-zenoh figure was the always-on
web server and REST/WebSocket driver.

## The base path is *also* heavier than mavlink-router (zenoh disabled)

Even with zenoh removed, a lean single-process router doing the same fan-out is
still much cheaper. The correct reference is BlueOS's other shipped router,
`mavlink-routerd` (mavlink-router v4, C++, **glibc** so it gets the VDSO) — not
`mavproxy`, which was the original misnomer.

### Head-to-head: no zenoh, with web/REST (identical live load)

Both run with the same master (`udps:127.0.0.1:8852`, fed by the same ArduPilot
navigator) and the same UDP/TCP fan-out (no zenoh — mavlink-router has no zenoh
support). mavlink-server still has the default REST driver and `:8080` web
server; the autopilot `run-service` watchdog was paused:

| Router                              | CPU (task-clock) | threads | ctx-sw/10s |
|-------------------------------------|------------------|---------|------------|
| `mavlink-server` (no zenoh, with web) | ~6.5%          | 5       | 4,424      |
| `mavlink-routerd` v4                  | ~2.4%          | 2       | 969        |

**mavlink-server costs ~2.7× the CPU of mavlink-router** in this configuration,
with ~4.6× the context switches and 5 threads vs 2 — for byte-identical routing
work.

### Head-to-head: no zenoh, no web/REST — true 1:1 routing (`wip2`)

Early single-sample measurement (2026-07-04, `f72519c`, multi-thread build):
~4.0% vs ~2.2% (~1.8×). **Stats harness at `50ffb99` (2026-07-05, n=75):**
lean-router **4.49% ±0.12%** vs `mavlink-routerd` **1.95% ±0.07%** (+2.3×).
See § "0.9.0 vs wip2 vs wip3" for the full table.

Re-measured 2026-07-04 on branch `wip2` @
`f72519cc9b2fcf89f3fc86f5f17c6add42bdd589`, which adds `--no-web` to skip the
`:8080` web server and the default REST/WebSocket driver. Same master and UDP/TCP
fan-out; `dev/benchmark-lean-router.sh` in the container (8 s warmup, 10 s
`pidstat` + `perf stat`):

| Router                                 | pidstat %CPU | task-clock / 10s | ctx-sw/10s | threads |
|----------------------------------------|--------------|------------------|------------|---------|
| `mavlink-server --no-web`              | ~4.0%        | ~510 ms (~5.1%)  | 5,313      | 5       |
| `mavlink-routerd` v4 (`/tmp/mavlink-routerd`) | ~2.2% | ~256 ms (~2.6%)  | 1,033      | 1       |

**Ratio: ~1.8× (pidstat) to ~2.0× (task-clock).** Context switches remain
~5× higher (5 tokio workers + broadcast fan-out vs one epoll thread). Stripping
web/REST saves ~1.5–2.5% CPU relative to the no-zenoh-with-web figure above.

> These `mavlink-server` figures are the **multi-thread** build. The current
> `current_thread` build measures **3.45% / 1576 ctx-sw / 1 thread** for the same
> lean config — see the `current_thread` section for the updated head-to-head.

Lean `mavlink-server` command:

```sh
mavlink-server --no-web \
  udps:127.0.0.1:8852 udpc:127.0.0.1:14000 tcps:127.0.0.1:5777 \
  udpc:192.168.2.1:14550 udps:127.0.0.1:14001 \
  udps:0.0.0.0:11001 udps:0.0.0.0:14660
```

Equivalent `mavlink-routerd` command:

```sh
mavlink-routerd -t 5777 -e 127.0.0.1:14000 -e 192.168.2.1:14550 \
  127.0.0.1:8852 127.0.0.1:14001 0.0.0.0:11001 0.0.0.0:14660
```

### Why mavlink-server > mavlink-router (base path)

The reasons apply to both head-to-head configurations; the with-web run adds
REST/WebSocket JSON broadcast subscribers on top:

- **Multi-threaded runtime coordination (largest structural difference on the
  multi-thread build — now mitigated).** With `multi_thread` there were 4 tokio
  workers plus the broadcast fan-out: each received message is cloned into a
  broadcast channel and wakes a task per endpoint, frequently on another core →
  `futex` wakeups + context switches + cache-line bouncing (**4,424 ctx-sw/10s
  vs 969** for routerd). Switching to `current_thread` removes the cross-thread
  wakeups (ctx-sw ~4600 → ~1576 on the lean build), leaving the remaining gap to
  `mavlink-routerd` dominated by the syscall/`sysinfo`/musl-clock items below.
  mavlink-router does the whole receive-and-fan-out in essentially one epoll
  loop.
- **musl static build has no VDSO** → every clock read is a real syscall.
  `strace` shows `clock_gettime64` firing **~3,400×/s**. The glibc
  `mavlink-routerd` reads the clock from userspace for ~free. Pure, per-message
  overhead of the musl build (tokio timers, timestamps, tracing).
- **Continuous `/proc` thread enumeration.** `strace -e open,getdents64` on the
  live process shows it repeatedly walking its own thread tree:
  `/proc/<pid>/task/<tid>/task` **~187×/s** and `/proc/<pid>/task` **~102×/s**
  (plus the `open`/`getdents64`/ENOENT bursts). A `sysinfo`-style process/thread
  metrics refresh loop running far too often — a router should never scan `/proc`
  on a hot timer.
- **`/etc/localtime` re-read ~3×/s** for timestamp formatting instead of caching
  the timezone once. Minor, but again something the lean router does not do.
- **Per-message decode + re-encode.** mavlink-server parses each packet into a
  structured `Protocol` and re-serializes per endpoint / for sysid-compid
  routing; a lean router forwards the bytes with minimal parsing. Part of the
  ~50% user-side cost.

Net: the zenoh JSON driver is the single biggest lever, but it is not the only
reason mavlink-server is heavier. On the original multi-thread build the wakeup
model, the musl-no-VDSO clock syscalls, and the `/proc` metrics scan kept the
lean routing base ~2× costlier than `mavlink-routerd`. Switching to
`current_thread` removes the wakeup churn and brings the lean gap down to
**~1.9×**; the remainder is now the musl clock syscalls, the `/proc` scan, and
per-message decode/re-encode. The default web/REST stack adds another
~1.5–2.5% on top (bringing the earlier no-zenoh figure to ~2.7×).

### Reference binary

`mavlink-routerd` was fetched as the prebuilt glibc armhf release BlueOS used to
ship:
`https://github.com/mavlink-router/mavlink-router/releases/download/v4/mavlink-routerd-glibc-armhf`.
(MAVProxy could not be installed cheaply on this box: its deps `numpy`/`pymavlink`
must compile from source on `armv7`, and the container's custom
`/usr/local/bin/python3` does not see Debian `dist-packages`.)

## Production rollout: AF_UNIX loopback (Stage 3)

The `unix:` / `unixs:` drivers bypass the IP loopback stack for co-located links.
To replace the internal `tcps:127.0.0.1:5777` endpoint in BlueOS, change
`ardupilot_manager` (`MAVLinkServer.py`) to use a filesystem socket path, e.g.
`unix:///run/mavlink-server/internal.sock` (client) paired with
`unixs:///run/mavlink-server/internal.sock` (server-side listener in the
co-located consumer). Coordinate path permissions and socket cleanup with the
consumer process; benchmark after deploy with `dev/benchmark-lean-router.sh`.

## Recommendations (ranked by measured impact)

**Applied and committed (2026-07-06):**

- **tracing filter** (`logger.rs`): file/server layers no longer default to
  `DEBUG` → `tracing_subscriber` self-time 6% → 2% on-CPU (**~0.5% absolute**).
- **zenoh publisher cache** (`zenoh/json.rs`): cache `Publisher` by
  `(sys,comp,msgid)` instead of `format!`+lookup per message/field →
  `__libc_malloc_impl` 0.91% → 0.35% on-CPU (**~0.1–0.15% absolute**).

**Applied and committed (2026-07-07):**

- **`--no-web` alignment** (`20cc54c`): removed `start_lean()` and lean-only
  50 ms clock; `--no-web` only disables web/REST/zenoh drivers. Restores fair
  lean-vs-production comparison and microsecond timestamps.
- **`sysinfo` removal** (`resources.rs`): `/proc` reads instead of
  `sysinfo::refresh_processes_specifics` → **`open`/`getdents64` ~600/s → 0**;
  production **−1.1% absolute** CPU (7.50% → 6.48% with WS client, n=30). (§
  "Applied: sysinfo and ingress clock dedup")
- **Ingress clock dedup** (`note_ingress`): reuse `message.timestamp` for
  immediate post-creation stats → **`clock_gettime` −11%** (−406/s) with no change
  to egress delay measurement. (§ "Applied: sysinfo and ingress clock dedup")

**Measured and ruled out:**

- **Zenoh SHM / UDP** won't help — MAVLink JSON payloads are <3072 B so SHM never
  activates by design; cost is per-op overhead × op-count + two-hop routing, not
  byte copying. (§ "Zenoh deep-dive")
- **Per-field fan-out** is *not* the main lever — dropping 78% of puts saves only
  **~1.7%/core combined** (mavlink-server ~0.5, zenohd ~1.2). (§ "Zenoh deep-dive")
- **`start_lean()` / 50 ms lean clock** — invalid lean tuning, not comparable to
  production; removed in `20cc54c`. (§ "Invalidated: --no-web lean tuning")
- **50 ms unified clock cache** — invalid benchmark + breaks 1 µs jitter.
  (§ "Invalidated: cached-clock shortcut")
- **1 ms cached clock refresher** — also quantizes sub-ms jitter; ruled out with
  direct `SystemTime` reads in `20cc54c`.

**Hard constraint (non-negotiable):** every current production feature stays.
The **zenoh JSON driver**, the **raw zenoh driver (`zenohraw`)**, **all message
and per-field topics**, and **microsecond** delay/jitter timestamps are **required** — a router plus
at least one recording client always listen to every topic. Levers that *drop*
or make features *lazy/opt-in* are **off the table**. The goal is to make the
*same* work cheaper: fewer syscalls/allocations per operation, cheaper
per-operation and per-hop cost, and lower runtime-coordination overhead.
Benchmarks must include **active WebSocket clients** when measuring production
web/REST cost.

> Ruled out as violating the constraint (kept here only to document the measured
> cost of each mandatory feature): making JSON publishing lazy/opt-in
> (**~4.5%/core combined**: mavlink-server ~1.7 + zenohd ~2.8), removing the raw
> driver (**~3.95%/core combined**: ~2.2 + ~1.8), or dropping per-field topics
> (**~1.7%/core combined**). These are the price of the required features, not
> optimization targets.

**Open levers that keep every feature (ranked by measured/expected impact):**

1. **Reduce the per-operation and per-hop cost of the mandatory zenoh traffic.**
   ~1000 puts/s cost ~8.5%/core system-wide (mavlink-server + `zenohd`), and it
   is **per-operation overhead × op-count + two-hop routing**, not byte copying
   (SHM never engages, § "Zenoh deep-dive"). Investigate **batching** several
   `put`s per network flush, cheaper serialization, and lowering the client↔router
   hop cost — all while still publishing every topic. Continue the applied
   micro-optimizations (tracing filter, publisher cache) in the same vein.
2. **Prefer UDP over TCP for the internal loopback link** (`tcps:5777` →
   `udp`). Transport swap, keeps the endpoint. Expected: ~2% saving.
3. **Re-benchmark full production under post-`927f47a7` layout** (n=75 stats
   harness, WS client, zenoh working) to revise the **13.09%** wip4d headline.

Base-path fixes (close the gap vs a lean router, apply after the above):

4. **Cache the timezone** so timestamp formatting stops re-reading
   `/etc/localtime`.

Not worth doing / already ruled out:

- **`/proc` scan via `sysinfo`** — fixed in post-`20cc54c` build; see § "Applied:
  sysinfo and ingress clock dedup". (Was open item #4.)
- **`start_lean()` or a separate `--no-web` runtime** — removed; invalid lean
  comparison. (§ "Invalidated: --no-web lean tuning")
- **Cached `now_micros()` at any refresher interval** — microsecond jitter
  requires direct clock reads on musl; cache ruled out. (§ "Invalidated:
  cached-clock shortcut")
- Tuning tokio `worker_threads` count on the **multi-thread** flavor (no
  meaningful benefit measured; 1 worker even increased context switches).
- The per-message stats `RwLock`→atomics change was correctly measured as
  irrelevant — bookkeeping is not on the hot path.

## Reproducing

On the Pi, with the autopilot navigator running and streaming to
`udps:127.0.0.1:8852` but no router attached, run `mavlink-server` manually with
the production endpoint list and measure with:

```sh
pidstat -p "$PID" 2 8              # average %CPU
perf stat -p "$PID" -- sleep 10    # context-switches / task-clock
strace -f -c -p "$PID"             # syscall counts (read counts, not times)
perf record -F 999 -p "$PID" -g -- sleep 12 && perf report --stdio
```

Attribute cost by re-running with endpoint subsets removed (drop `zenoh:*`,
`zenohraw:*`, `tcps:*`, etc.) and comparing `%CPU`.

### Automated benchmarks (`dev/benchmark-*.sh`)

Inside `blueos-core`, after deploying the binary:

```sh
# Fetch mavlink-routerd if not present
curl -fsSL -o /tmp/mavlink-routerd \
  https://github.com/mavlink-router/mavlink-router/releases/download/v4/mavlink-routerd-glibc-armhf
chmod +x /tmp/mavlink-routerd

# Full suite (codec-impact + lean-router, recommended):
# Deploy binary to /usr/bin/mavlink-server, then:
bash dev/run-bench-090.sh   # or run-bench-wip2.sh / run-bench-wip3.sh / run-bench-wip4d.sh
# Defaults in suite runners: BENCH_TRIALS=5 BENCH_SAMPLES=15 (n=75/case)

# Or run scripts individually:
BENCH_TRIALS=5 BENCH_SAMPLES=15 BENCH_INTERVAL_SECS=2 WARMUP_SECS=10 \
  bash dev/benchmark-codec-impact.sh

# Lean 1:1 only
BENCH_TRIALS=5 BENCH_SAMPLES=15 bash dev/benchmark-lean-router.sh

# Lean routing syscall attribution (strace + pidstat -w)
# Copy strace into container first: docker cp /usr/bin/strace blueos-core:/tmp/strace
BENCH_TRIALS=1 WARMUP_SECS=10 MEASURE_SECS=10 STRACE_BIN=/tmp/strace \
  bash dev/profile-lean-strace.sh
MEASURE_SECS=5 STRACE_BIN=/tmp/strace bash dev/profile-lean-writes.sh

# Production syscall attribution (manager-spawned PID, zenoh + web)
STRACE_BIN=/tmp/strace WARMUP_SECS=5 MEASURE_SECS=10 bash dev/profile-prod-strace.sh

# Production with REST WebSocket client (exercises JSON broadcast path)
STRACE_BIN=/tmp/strace WARMUP_SECS=10 MEASURE_SECS=10 bash dev/profile-prod-with-ws.sh
```

Each case prints a summary line:

```
STATS: <label> n=50 mean=8.42% std=0.31% min=7.80% max=9.10% ci95=±0.09%
```

- **`n`** — total pidstat samples ( `BENCH_TRIALS` × `BENCH_SAMPLES`; isolated
  cases restart the process each trial; production re-samples the same PID).
- **`mean` / `std`** — sample mean and standard deviation of `%CPU`.
- **`ci95`** — normal-approximation half-width: `1.96 × std / sqrt(n)`.
- Treat two configs as **significantly different** only when their `ci95`
  intervals do not overlap **and** preflight reported exactly one live navigator.

#### Tunables (`dev/benchmark-common.sh`)

| Variable | Default | Role |
|----------|---------|------|
| `BENCH_TRIALS` | 3 | Independent runs per case (process restart for bench copies) |
| `BENCH_SAMPLES` | 10 | `pidstat` samples collected per trial |
| `BENCH_INTERVAL_SECS` | 2 | Seconds between `pidstat` samples |
| `BENCH_TRIAL_GAP_SECS` | 3 | Pause between trials |
| `WARMUP_SECS` | 10 | Steady-state delay after process start |
| `BENCH_MIN_NAVIGATORS` / `BENCH_MAX_NAVIGATORS` | 1 / 1 | Abort if firmware count is wrong |

**Minimum for significance:** use `BENCH_TRIALS≥3` and `BENCH_SAMPLES≥10` (≥30
samples per case). Prefer **`BENCH_TRIALS=5` `BENCH_SAMPLES=15`** (75 samples,
~95 s/case) when comparing releases — used for the § "0.9.0 vs wip2 vs wip3"
table.

`dev/benchmark-common.sh` is sourced by both scripts. It **pauses
`ardupilot_manager` with `SIGSTOP`** (and resumes it with `SIGCONT` on cleanup)
so the manager cannot spawn a second instance while bench cases run — the
navigator source keeps streaming and the real `/usr/bin/mavlink-server` is never
overwritten. **`benchmark_restore_managed` also kills any `ardupilot_navigator`
children** before restarting supervision, so orphaned firmware copies do not
accumulate across runs. **Preflight aborts unless exactly one live navigator
binary** is running (shell wrappers excluded). Each bench case runs a fresh
snapshot of the currently deployed binary (copied to `/tmp/mavlink-server-bench`,
md5 logged). Production is measured on the manager-spawned PID only. Capture
`LEAN_ARGS` after `benchmark_prepare_bench_binary` and
`benchmark_cache_production_cmdline` before isolation in `benchmark-codec-impact.sh`.

**Concurrency / duplicate prevention (2026-07-05):**

- `flock` on `/tmp/benchmark.lock` — only one benchmark suite at a time; refuses
  to start if another holds the lock.
- Run full suites via `dev/run-bench-*.sh` (foreground, holds lock) or a single
  `profile-lean-*.sh` invocation — not overlapping manual manager restarts.
- For long multi-script suites inside the container, `docker exec -d` is acceptable
  **only** when the whole suite holds the lock and restores on exit; an SSH
  disconnect must not leave a paused manager or duplicate instances.
- Isolated phase sets `BENCHMARK_ISOLATED=1`; preflight will not call restore
  mid-suite (which previously spawned a second production instance).
- Before each bench trial: assert `mavlink-server` count is **0**; after start
  assert count is **1**; after kill wait until count is **0** again.
- `benchmark_kill_all_managers` sends `SIGCONT` then `SIGKILL` to paused
  managers so restore cannot leave a stale paused manager alive.
- `dev/run-bench-090.sh` / `dev/run-bench-wip2.sh` / `dev/run-bench-wip3.sh`
  / `dev/run-bench-wip4d.sh`
- `dev/profile-prod-strace.sh` — production syscall profile (manager PID)
  run codec-impact and lean-router under one lock with an explicit
  `benchmark_restore_managed` between scripts (child scripts use
  `BENCHMARK_NO_RESTORE_ON_EXIT=1` so EXIT traps do not
  race with the next script).

### Lean 1:1 comparison vs mavlink-routerd (manual)

```sh
mavlink-server --no-web udps:127.0.0.1:8852 udpc:127.0.0.1:14000 \
  tcps:127.0.0.1:5777 udpc:192.168.2.1:14550 udps:127.0.0.1:14001 \
  udps:0.0.0.0:11001 udps:0.0.0.0:14660
```
