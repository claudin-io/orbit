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

fn init_tracing(log_path: &Path, debug: bool) -> anyhow::Result<WorkerGuard> {
    let log_dir = log_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let file_name = log_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "orbit-run.log".to_string());
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::never(log_dir, file_name);
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

    // The usage log goes to a temp file: deleted on success, kept on failure so
    // it can be attached to a bug report.
    let log_path = orbit::report::temp_log_path();
    let _guard = init_tracing(&log_path, debug)?;

    // Explicit `--config` path (Run only), needed by the failure report.
    let explicit_config = match &cli.command {
        orbit::cli::Command::Run { config, .. } => config.clone(),
        _ => None,
    };

    // Capture panics anywhere in the process: write a report (control may not
    // return past the unwind) and point the user at it, then run the default hook.
    install_panic_hook(log_path.clone(), target.clone(), explicit_config.clone());

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

    let result = orchestrator::dispatch(cli, events_tx).await;
    render_handle.await.ok();

    // Flush the non-blocking appender before reading or removing the log file.
    drop(_guard);

    match result {
        Ok(()) => {
            // Success: drop the temp usage log.
            let _ = std::fs::remove_file(&log_path);
            Ok(())
        }
        Err(e) if matches!(e, orbit::error::OrbitError::Exhausted(_)) => {
            // Expected outcome (loop didn't converge), not a defect: no report.
            let _ = std::fs::remove_file(&log_path);
            Err(e.into())
        }
        Err(e) => {
            let msg = e.to_string();
            match orbit::report::write_report(
                &log_path,
                &target,
                explicit_config.as_deref(),
                &msg,
            ) {
                Ok(report_path) => orbit::report::print_failure_notice(&report_path, &msg),
                Err(_) => eprintln!("Error: {}", msg),
            }
            std::process::exit(e.exit_code());
        }
    }
}

fn install_panic_hook(
    log_path: std::path::PathBuf,
    target: std::path::PathBuf,
    explicit_config: Option<String>,
) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("panic: {}", info);
        if let Ok(report_path) =
            orbit::report::write_report(&log_path, &target, explicit_config.as_deref(), &msg)
        {
            orbit::report::print_failure_notice(&report_path, &msg);
        }
        default_hook(info);
    }));
}
