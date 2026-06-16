use clap::Parser;
use orbit::cli::Cli;
use orbit::events;
use orbit::orchestrator;
use std::io::IsTerminal;
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

fn init_tracing(target: &Path, debug: bool) -> anyhow::Result<WorkerGuard> {
    let log_dir = target.join(".orbit").join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::never(log_dir, "orbit.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // File layer keeps its existing behavior: RUST_LOG, else `info`.
    let file_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(file_filter);

    // In debug mode, also stream orbit's own debug logs to stderr so they don't
    // corrupt the renderer's stdout drawing.
    let stderr_layer = debug.then(|| {
        let stderr_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("orbit=debug"));
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
            .compact()
            .with_filter(stderr_filter)
    });

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .init();
    Ok(guard)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version") {
        println!("{}", orbit::cli::ORBIT_VERSION);
        return Ok(());
    }
    let cli = Cli::parse();

    if matches!(
        cli.command,
        orbit::cli::Command::Run { .. } | orbit::cli::Command::Git { .. } | orbit::cli::Command::Config
    ) {
        orbit::render::print_banner(orbit::cli::ORBIT_VERSION);
    }

    let target = match &cli.command {
        orbit::cli::Command::Run { target, .. } => target
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
        orbit::cli::Command::Acp { .. }
        | orbit::cli::Command::Git { .. }
        | orbit::cli::Command::Config => std::env::current_dir().unwrap_or_default(),
    };
    let debug = cli.debug;
    let _guard = init_tracing(&target, debug)?;
    let (events_tx, events_rx) = events::channel();

    let verbose = debug
        || matches!(&cli.command, orbit::cli::Command::Run { verbose, .. } if *verbose);
    let render_handle = tokio::spawn(async move {
        let mut renderer = orbit::render::Renderer::new(verbose);
        let mut rx = events_rx;
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(80));
        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Some(event) => {
                            let is_done = matches!(event, orbit::types::OrbitEvent::RunFinished { .. });
                            renderer.handle(event);
                            if is_done { break; }
                        }
                        None => break,
                    }
                }
                _ = tick.tick() => {
                    renderer.tick();
                }
            }
        }
    });

    orchestrator::dispatch(cli, events_tx).await?;
    render_handle.await.ok();
    Ok(())
}
