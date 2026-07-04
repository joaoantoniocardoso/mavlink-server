# Stats benchmark baseline (before optimization)

Baseline for the per-message stats hot path, captured before replacing the
per-message `RwLock` writes with atomics and before dropping the dedicated hub
stats subscriber task.

## Environment

- Commit: `15e964d0955a73614ed9fb38f0380c0d026ab282`
- Date: 2026-07-04T14:38:28Z
- CPU: AMD Ryzen 9 9950X 16-Core Processor (32 threads)
- Profile: `bench` (release, `criterion`, 100 samples)
- Batch: 1000 messages per iteration (throughput is per message)

Reproduce with:

```bash
SKIP_FRONTEND=1 cargo bench --bench stats_bench -- --save-baseline before
```

## Results

Time is per iteration (1000 messages); per-message figures in the last column.

| Benchmark        | time (median) | per message |
|------------------|---------------|-------------|
| stats/driver_update | 55.596 µs  | 55.6 ns  |
| stats/hub_update    | 140.08 µs  | 140.1 ns |
| stats_route/0       | 202.39 µs  | 202.4 ns |
| stats_route/1       | 262.53 µs  | 262.5 ns |
| stats_route/3       | 383.24 µs  | 383.2 ns |
| stats_route/5       | 494.29 µs  | 494.3 ns |
| stats_route/10      | 774.41 µs  | 774.4 ns |
| stats_route/20      | 1.3259 ms  | 1325.9 ns |

Notes:

- `driver_update`: one locked `AccumulatedDriverStatsInner::update_input` per message.
- `hub_update`: the two locked hub updates per message (`hub_stats` + `hub_messages_stats`).
- `stats_route/N`: full per-message stats cost of routing to `N` outputs
  (1 input update + 2 hub updates + N output updates), each behind its own lock.
- Per-message cost scales roughly linearly with the number of outputs, as
  expected from `2 + M` locked updates per message.
