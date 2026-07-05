use anyhow::*;
use mavlink_server::{cli, hub, logger, runtime::PlaneRuntimes, web};
use tracing::*;

fn main() -> Result<()> {
    cli::init();

    let runtimes = PlaneRuntimes::start();

    runtimes.block_on(async_main())
}

async fn async_main() -> Result<()> {
    logger::init(cli::log_path(), cli::is_verbose(), cli::is_tracing());

    info!(
        "{}, version: {}-{}, build date: {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        env!("VERGEN_GIT_SHA"),
        env!("VERGEN_BUILD_DATE")
    );
    info!(
        "Starting at {}",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
    );
    debug!("Command line call: {}", cli::command_line_string());
    debug!("Command line input struct call: {}", cli::command_line());

    hub::init();

    for driver in cli::endpoints() {
        hub::add_driver(driver).await?;
    }

    if cli::no_web() {
        tokio::signal::ctrl_c().await?;
    } else {
        web::run(cli::web_server()).await;
    }

    for (id, driver_info) in hub::drivers().await? {
        debug!("Removing driver id {id:?} ({driver_info:?})");
        hub::remove_driver(id).await?;
    }

    Ok(())
}
