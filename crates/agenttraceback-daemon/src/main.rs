use std::{
    fs,
    process::ExitCode,
    time::{Duration, SystemTime},
};

use agenttraceback_config::PlatformPaths;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("agenttracebackd=info,agenttraceback_api=info"));
    let paths = match PlatformPaths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("agenttracebackd could not resolve platform paths: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = paths.ensure_directories() {
        eprintln!("agenttracebackd could not create application directories: {error}");
        return ExitCode::FAILURE;
    }
    cleanup_old_logs(&paths.logs_root, Duration::from_secs(7 * 24 * 60 * 60));
    let file_appender = tracing_appender::rolling::daily(&paths.logs_root, "agenttraceback.log");
    let (log_writer, _log_guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(log_writer)
        .init();

    match agenttraceback_daemon::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(agenttraceback_daemon::DaemonError::AlreadyRunning) => {
            eprintln!("agenttracebackd is already running for this data directory");
            ExitCode::from(5)
        }
        Err(error) => {
            eprintln!("agenttracebackd failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cleanup_old_logs(logs_root: &std::path::Path, retention: Duration) {
    let Ok(entries) = fs::read_dir(logs_root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_agent_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("agenttraceback.log"));
        if !is_agent_log {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > retention);
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}
