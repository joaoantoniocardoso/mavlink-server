# Stats benchmark (after optimization)

Results after replacing the per-message async `RwLock` stats writes with
lock-free atomic accumulators and dropping the dedicated hub stats subscriber
task (folding hub accounting inline into the publish paths).

Measured with the **same harness** as the baseline (tokio `Runtime` +
`block_on` + `Arc` + BATCH loop); only the inner stats operation changed, so the
comparison isolates the accumulator cost.

## Environment

- CPU: AMD Ryzen 9 9950X 16-Core Processor (32 threads)
- Profile: `bench` (release, `criterion`, 100 samples)
- Batch: 1000 messages per iteration (throughput is per message)

Reproduce with:

```bash
SKIP_FRONTEND=1 cargo bench --bench stats_bench -- --baseline before
```

## Results

| Benchmark           | before (median) | after (median) | change   |
|---------------------|-----------------|----------------|----------|
| stats/driver_update | 55.596 µs       | 38.332 µs      | -30.9 %  |
| stats/hub_update    | 140.08 µs       | 85.204 µs      | -39.2 %  |
| stats_route/0       | 202.39 µs       | 124.18 µs      | -38.7 %  |
| stats_route/1       | 262.53 µs       | 160.50 µs      | -38.7 %  |
| stats_route/3       | 383.24 µs       | 234.69 µs      | -39.0 %  |
| stats_route/5       | 494.29 µs       | 308.74 µs      | -37.6 %  |
| stats_route/10      | 774.41 µs       | 493.11 µs      | -36.1 %  |
| stats_route/20      | 1.3259 ms       | 859.92 µs      | -34.8 %  |

## What changed

- `AtomicAccumulatedStats`: pure `AtomicU64` counters (`messages`, `bytes`,
  `delay`, `last_update_us`) updated with relaxed atomics; no lock, no `.await`.
- Dropped the never-read `last_message: Arc<Protocol>` from the hot path (it was
  write-only in the previous accumulators; only the derived `StatsInner` is ever
  consumed), removing an `Arc` clone per message.
- `AtomicHubMessagesStats`: per-message-id map behind a `std::sync::RwLock`;
  updates take a shared read lock + atomic increment, and only take the write
  lock the first time a `(system, component, message)` triple is seen.
- Driver stats moved from `Arc<RwLock<AccumulatedDriverStats>>` to
  `Arc<AtomicDriverStats>`.
- The dedicated hub stats subscriber task was removed; hub accounting is now done
  inline via `hub::accumulate_hub_message` on every path that publishes to the
  hub broadcast.

## Caveat

This is a single-threaded, uncontended microbenchmark. It does **not** capture
the larger production win of removing cross-thread lock contention and async
scheduler wakeups, so it understates the real-world improvement.
