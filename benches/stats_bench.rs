use std::sync::Arc;

use clap::Parser;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mavlink_server::{
    cli,
    protocol::Protocol,
    stats::accumulated::{
        AccumulatedStatsInner, driver::AccumulatedDriverStatsInner,
        messages::AccumulatedHubMessagesStats,
    },
};
use tokio::{runtime::Runtime, sync::RwLock};

/// Number of messages recorded per benchmark iteration. Amortizes the
/// `block_on` overhead so the reported per-element time reflects the stats
/// update cost itself.
const BATCH: u64 = 1000;

fn init_cli() {
    cli::init_with(cli::Args::parse_from(vec![
        std::env::args().next().unwrap_or_default(),
        "--allow-no-endpoints".to_string(),
    ]));
}

fn sample_message() -> Arc<Protocol> {
    let header = mavlink::MavHeader::default();
    let message =
        mavlink::ardupilotmega::MavMessage::HEARTBEAT(mavlink::ardupilotmega::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::ardupilotmega::MavType::MAV_TYPE_ONBOARD_CONTROLLER,
            autopilot: mavlink::ardupilotmega::MavAutopilot::MAV_AUTOPILOT_INVALID,
            base_mode: mavlink::ardupilotmega::MavModeFlag::empty(),
            system_status: mavlink::ardupilotmega::MavState::MAV_STATE_STANDBY,
            mavlink_version: 0x3,
        });

    Arc::new(Protocol::from_mavlink_raw(header, &message, "bench"))
}

/// Per-message driver stats update, as done once on receive and once per output
/// driver on send in the drivers' hot loops.
fn bench_driver_update(c: &mut Criterion) {
    init_cli();
    let message = sample_message();
    let rt = Runtime::new().unwrap();
    let stats = Arc::new(RwLock::new(AccumulatedDriverStatsInner::default()));

    let mut group = c.benchmark_group("stats");
    group.throughput(Throughput::Elements(BATCH));
    group.bench_function("driver_update", |b| {
        b.iter(|| {
            rt.block_on(async {
                for _ in 0..BATCH {
                    stats.write().await.update_input(&message);
                }
            });
        });
    });
    group.finish();
}

/// Per-message hub stats update, as done by the dedicated hub stats subscriber
/// task (two independent locked updates per message).
fn bench_hub_update(c: &mut Criterion) {
    init_cli();
    let message = sample_message();
    let rt = Runtime::new().unwrap();
    let hub_stats = Arc::new(RwLock::new(AccumulatedStatsInner::default()));
    let hub_messages_stats = Arc::new(RwLock::new(AccumulatedHubMessagesStats::default()));

    let mut group = c.benchmark_group("stats");
    group.throughput(Throughput::Elements(BATCH));
    group.bench_function("hub_update", |b| {
        b.iter(|| {
            rt.block_on(async {
                for _ in 0..BATCH {
                    hub_stats.write().await.update(&message);
                    hub_messages_stats.write().await.update(&message);
                }
            });
        });
    });
    group.finish();
}

/// Full per-message stats cost of routing one message to `outputs` endpoints:
/// one input driver update, two hub updates, and one update per output driver,
/// each behind its own lock (matching the real per-driver stats ownership).
fn bench_route(c: &mut Criterion) {
    init_cli();
    let message = sample_message();

    let mut group = c.benchmark_group("stats_route");
    for outputs in [0u64, 1, 3, 5, 10, 20] {
        group.throughput(Throughput::Elements(BATCH));
        group.bench_with_input(
            BenchmarkId::from_parameter(outputs),
            &outputs,
            |b, &outputs| {
                let rt = Runtime::new().unwrap();
                let input_stats = Arc::new(RwLock::new(AccumulatedDriverStatsInner::default()));
                let hub_stats = Arc::new(RwLock::new(AccumulatedStatsInner::default()));
                let hub_messages_stats =
                    Arc::new(RwLock::new(AccumulatedHubMessagesStats::default()));
                let output_stats: Vec<_> = (0..outputs)
                    .map(|_| Arc::new(RwLock::new(AccumulatedDriverStatsInner::default())))
                    .collect();

                b.iter(|| {
                    rt.block_on(async {
                        for _ in 0..BATCH {
                            input_stats.write().await.update_input(&message);
                            hub_stats.write().await.update(&message);
                            hub_messages_stats.write().await.update(&message);
                            for output in &output_stats {
                                output.write().await.update_output(&message);
                            }
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_driver_update, bench_hub_update, bench_route);
criterion_main!(benches);
