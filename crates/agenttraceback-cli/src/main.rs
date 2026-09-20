use std::{
    env,
    ffi::OsString,
    fs,
    io::Write,
    path::Path,
    path::PathBuf,
    process::{Command as ProcessCommand, ExitCode, Stdio},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agenttraceback_adapter_sdk::{HookPlan, HookReceipt};
use agenttraceback_api::{
    AdapterImportResponse, AdapterScanResponse, CompleteWrapperRequest, DemoDataView,
    ExecuteRecoveryRequest, ExportRequest, ExportView, HealthResponse, InstallAdapterHookRequest,
    PlanRecoveryRequest, PrepareWrapperRequest, PrepareWrapperResponse, RecoveryPlanView,
    RecoveryRunView, RegisterRootPidRequest, SessionView, UninstallAdapterHookRequest,
    VerificationView,
};
use agenttraceback_config::PlatformPaths;
use agenttraceback_platform::{
    PtyError, PtyOptions, install_startup, remove_startup, run_pty, startup_status,
};
use agenttraceback_types::{API_VERSION, EventEnvelope, RuntimeMetadata};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(
    name = "agenttraceback",
    version,
    about = "Verified record and recovery for AI coding-agent runs"
)]
struct Cli {
    /// Emit stable machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the desktop application.
    Open,
    /// Query daemon, database, and capture health.
    Status,
    /// Print redacted diagnostics for support.
    Doctor {
        /// Emit stable machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Write a redacted support bundle to this path.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Search redacted event history through the local API.
    Search {
        /// Structured or full-text query.
        query: String,
        /// Maximum number of results.
        #[arg(long, default_value_t = 50)]
        limit: u16,
    },
    /// Launch a command in a recorded interactive PTY.
    Run {
        /// Project path; defaults to the current directory.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Human-readable session label.
        #[arg(long)]
        label: Option<String>,
        /// Agent name for attribution and display.
        #[arg(long)]
        agent: Option<String>,
        /// Disable terminal transcript capture.
        #[arg(long)]
        no_transcript: bool,
        /// Skip the pre-session baseline snapshot.
        #[arg(long)]
        no_recovery_snapshot: bool,
        /// Exact command argv after `--`.
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<OsString>,
    },
    /// Plan or execute safe recovery.
    Recover {
        #[command(subcommand)]
        command: RecoverCommand,
    },
    /// List pre/post file versions for a session.
    Files {
        /// Session identifier.
        session_id: String,
    },
    /// Export a redacted JSON or Markdown evidence bundle.
    Export {
        /// Session identifier.
        session_id: String,
        /// Export format.
        #[arg(long, default_value = "json")]
        format: String,
        /// Include explicitly decrypted eligible content in a local full export.
        #[arg(long)]
        full: bool,
    },
    /// List, scan, or import built-in agent adapters.
    Adapters {
        #[command(subcommand)]
        command: AdaptersCommand,
    },
    /// Print version and protocol compatibility information.
    Version,
    /// Generate shell completion for bash, zsh, fish, or powershell.
    Completions {
        /// Target shell.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// List recent sessions.
    Sessions {
        /// Maximum number of sessions.
        #[arg(long, default_value_t = 50)]
        limit: u16,
    },
    /// Show one session and timeline summary.
    Show {
        /// Session identifier.
        session_id: String,
        /// Maximum timeline events to include.
        #[arg(long, default_value_t = 200)]
        limit: u16,
    },
    /// Manage the local daemon process.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Install or remove the clearly labeled local demo dataset.
    Demo {
        #[command(subcommand)]
        command: DemoCommand,
    },
    /// Verify append-only integrity chains.
    Verify {
        /// Session identifier. Omit with `--all` for the global chain.
        session_id: Option<String>,
        /// Verify the daemon-global chain.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AdaptersCommand {
    /// List detected adapters.
    List,
    /// Rescan known adapter locations.
    Scan,
    /// Import historical records for one adapter.
    Import {
        /// Adapter identifier.
        adapter_id: String,
    },
    /// Plan, install, or remove optional agent hooks.
    Hooks {
        #[command(subcommand)]
        command: HookCommand,
    },
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    /// Show the exact reversible hook plan without changing files.
    Plan {
        /// Adapter identifier.
        adapter_id: String,
    },
    /// Install a plan after explicit digest approval.
    Install {
        /// Adapter identifier.
        adapter_id: String,
        /// Exact plan digest printed by `hooks plan`.
        #[arg(long)]
        approve: String,
    },
    /// Remove only AgentTraceback-owned hook entries.
    Uninstall {
        /// Adapter identifier.
        adapter_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Start the daemon if it is not already reachable.
    Start,
    /// Stop the daemon identified by current runtime metadata.
    Stop,
    /// Stop and start the daemon.
    Restart,
    /// Print daemon log locations.
    Logs,
    /// Manage per-user startup registration.
    Startup {
        #[command(subcommand)]
        command: StartupCommand,
    },
    /// Create a consistent SQLite backup while the daemon is running.
    Backup {
        /// Optional destination path.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Restore a verified backup after stopping the daemon.
    Restore {
        /// Backup database path.
        input: PathBuf,
        /// Required explicit confirmation.
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Debug, Subcommand)]
enum StartupCommand {
    /// Register the daemon to start at login.
    Install,
    /// Remove the per-user startup registration.
    Remove,
    /// Report startup registration state.
    Status,
}

#[derive(Debug, Subcommand)]
enum DemoCommand {
    /// Install deterministic sample data into the local store.
    Install,
    /// Remove all demo project history.
    Remove,
}

#[derive(Debug, Subcommand)]
enum RecoverCommand {
    /// Build an immutable recovery plan.
    Plan {
        /// Source session identifier.
        session_id: String,
        /// Recovery action.
        #[arg(long, default_value = "reconstruct_pre_session")]
        action: String,
        /// Destination directory or file.
        #[arg(long)]
        destination: PathBuf,
        /// Selected project-relative paths for partial restore.
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// Execute a previously prepared recovery plan.
    Execute {
        /// Plan identifier.
        plan_id: String,
        /// Explicitly confirm execution.
        #[arg(long)]
        confirm: bool,
        /// Overwrite files changed since plan preparation.
        #[arg(long)]
        overwrite_conflicts: bool,
    },
}

#[derive(Debug, Error)]
enum CliError {
    #[error("daemon is unavailable: {0}")]
    DaemonUnavailable(String),
    #[error("daemon returned HTTP {status}: {body}")]
    DaemonHttp { status: StatusCode, body: String },
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("local configuration failed: {0}")]
    Config(#[from] agenttraceback_config::ConfigError),
    #[error("could not encode JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("PTY failure: {0}")]
    Pty(#[from] PtyError),
    #[error("startup registration failed: {0}")]
    Startup(#[from] agenttraceback_platform::StartupError),
    #[error("database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("local I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl CliError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::DaemonUnavailable(_) | Self::Request(_) => 3,
            Self::DaemonHttp { .. } => 1,
            Self::Config(_)
            | Self::Json(_)
            | Self::Pty(_)
            | Self::Startup(_)
            | Self::Database(_)
            | Self::Io(_) => 1,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusOutput {
    reachable: bool,
    api_version: u32,
    runtime: RuntimeView,
    health: Option<HealthResponse>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeView {
    pid: u32,
    port: u16,
    daemon_version: String,
    api_version: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    items: Vec<SearchItem>,
    has_more: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchItem {
    event_id: String,
    occurred_at_us: i64,
    action: String,
    agent: Option<String>,
    model: Option<String>,
    project_name: Option<String>,
    target_display: Option<String>,
    redacted_preview: Option<String>,
    evidence: String,
    risk: String,
}

impl From<&RuntimeMetadata> for RuntimeView {
    fn from(metadata: &RuntimeMetadata) -> Self {
        Self {
            pid: metadata.pid,
            port: metadata.port,
            daemon_version: metadata.daemon_version.clone(),
            api_version: metadata.api_version,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("agenttraceback: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

async fn execute(cli: Cli) -> Result<ExitCode, CliError> {
    let json = cli.json;
    match cli.command {
        Command::Open => {
            open_desktop()?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Status => {
            status(json).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor { json, output } => {
            doctor(json, output).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Search { query, limit } => {
            search(json, &query, limit).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            project,
            label,
            agent,
            no_transcript,
            no_recovery_snapshot,
            command,
        } => {
            run_wrapper(
                json,
                project,
                label,
                agent,
                no_transcript,
                no_recovery_snapshot,
                command,
            )
            .await
        }
        Command::Recover { command } => {
            recover(json, command).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Files { session_id } => {
            list_files(json, &session_id).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Export {
            session_id,
            format,
            full,
        } => {
            export_session(json, &session_id, &format, full).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Adapters { command } => {
            adapters(json, command).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            version(json)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Completions { shell } => {
            let mut command = Cli::command();
            let mut output = Vec::new();
            generate(shell, &mut command, "agenttraceback", &mut output);
            if let Err(error) = std::io::stdout().write_all(&output) {
                if error.kind() == std::io::ErrorKind::BrokenPipe {
                    return Ok(ExitCode::SUCCESS);
                }
                return Err(CliError::Io(error));
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Sessions { limit } => {
            sessions(json, limit).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Show { session_id, limit } => {
            show_session(json, &session_id, limit).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Daemon { command } => {
            daemon_command(json, command).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Demo { command } => {
            demo(json, command).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify { session_id, all } => {
            let valid = verify_chain(json, session_id.as_deref(), all).await?;
            Ok(if valid {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(6)
            })
        }
    }
}

fn open_desktop() -> Result<(), CliError> {
    let executable = env::var_os("AGENTTRACEBACK_DESKTOP_BIN")
        .map(PathBuf::from)
        .or_else(|| sibling_binary("agenttraceback-desktop"))
        .ok_or_else(|| {
            CliError::DaemonUnavailable(
                "agenttraceback-desktop was not found next to this CLI. Set AGENTTRACEBACK_DESKTOP_BIN."
                    .to_owned(),
            )
        })?;
    ProcessCommand::new(&executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            CliError::DaemonUnavailable(format!("could not open {}: {error}", executable.display()))
        })?;
    Ok(())
}

async fn doctor(json: bool, output: Option<PathBuf>) -> Result<(), CliError> {
    let paths = PlatformPaths::discover()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut report = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "apiVersion": API_VERSION,
        "platform": env::consts::OS,
        "architecture": env::consts::ARCH,
        "dataRoot": paths.data_root,
        "configRoot": paths.config_root,
        "database": paths.database_file,
        "daemon": {"reachable": false},
        "checks": []
    });
    let mut checks = Vec::new();
    match paths.read_runtime_metadata() {
        Ok(runtime) => {
            let base_url = format!("http://127.0.0.1:{}", runtime.port);
            report["daemon"] = serde_json::json!({
                "reachable": true,
                "pid": runtime.pid,
                "port": runtime.port,
                "version": runtime.daemon_version,
            });
            let health: HealthResponse =
                get_json(&client, &base_url, "/api/v1/health", &runtime.token).await?;
            checks.push(("health", health.status == "ok", health.status));
            checks.push((
                "database",
                health.database.status == "ready",
                health.database.status,
            ));
            checks.push(("key_protection", true, health.key_protection));
        }
        Err(error) => {
            checks.push(("daemon", false, error.to_string()));
        }
    }
    report["checks"] = serde_json::Value::Array(
        checks
            .iter()
            .map(|(name, passed, detail)| {
                serde_json::json!({"name": name, "passed": passed, "detail": detail})
            })
            .collect(),
    );
    if let Some(output) = output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
        if !json {
            println!("Support bundle: {}", output.display());
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("AgentTraceback doctor");
        println!("Version: {}", env!("CARGO_PKG_VERSION"));
        println!("Data root: {}", paths.data_root.display());
        for (name, passed, detail) in checks {
            println!("{} {name}: {detail}", if passed { "ok" } else { "fail" });
        }
    }
    Ok(())
}

async fn sessions(json: bool, limit: u16) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(10))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let sessions: Vec<SessionView> = get_json(
        &client,
        &base_url,
        &format!("/api/v1/sessions?limit={limit}"),
        &runtime.token,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
    } else if sessions.is_empty() {
        println!("No sessions recorded.");
    } else {
        println!(
            "{:<36} {:<20} {:<12} {:>8} {:>7}",
            "SESSION", "AGENT", "STATE", "EVENTS", "FILES"
        );
        for session in sessions {
            println!(
                "{:<36} {:<20} {:<12} {:>8} {:>7}",
                session.id,
                session.agent_name.as_deref().unwrap_or("unknown"),
                session.state,
                session.event_count,
                session.file_count
            );
        }
    }
    Ok(())
}

async fn show_session(json: bool, session_id: &str, limit: u16) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(30))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let session: SessionView = get_json(
        &client,
        &base_url,
        &format!("/api/v1/sessions/{session_id}"),
        &runtime.token,
    )
    .await?;
    let events: Vec<EventEnvelope> = get_json(
        &client,
        &base_url,
        &format!("/api/v1/sessions/{session_id}/events?limit={limit}"),
        &runtime.token,
    )
    .await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session": session,
                "events": events,
            }))?
        );
    } else {
        println!("Session: {}", session.id);
        println!(
            "Title: {}",
            session.title_preview.as_deref().unwrap_or("Untitled")
        );
        println!(
            "Agent: {}",
            session.agent_name.as_deref().unwrap_or("unknown")
        );
        println!(
            "Model: {}",
            session.model_name.as_deref().unwrap_or("unknown")
        );
        println!("State: {}", session.state);
        println!("Outcome: {}", session.outcome);
        println!("Capture: {}", session.capture_health);
        println!("Recovery: {}", session.recovery_coverage);
        println!("Events: {}", session.event_count);
        println!("Files: {}", session.file_count);
        println!("Findings: {}", session.finding_count);
        if !events.is_empty() {
            println!("\nTimeline:");
            for event in events {
                println!(
                    "{} {:<16} {:<8} {}",
                    event.occurred_at_us,
                    event.action.as_str(),
                    event.evidence.class.as_str().to_uppercase(),
                    event
                        .target
                        .display
                        .as_deref()
                        .or(event.content.redacted_preview.as_deref())
                        .unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

async fn daemon_command(json: bool, command: DaemonCommand) -> Result<(), CliError> {
    let paths = PlatformPaths::discover()?;
    paths.ensure_directories()?;
    match command {
        DaemonCommand::Start => start_daemon(&paths, json).await,
        DaemonCommand::Stop => stop_daemon(&paths, json),
        DaemonCommand::Restart => {
            let _ = stop_daemon(&paths, json);
            tokio::time::sleep(Duration::from_millis(300)).await;
            start_daemon(&paths, json).await
        }
        DaemonCommand::Logs => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "logsRoot": paths.logs_root,
                    }))?
                );
            } else {
                println!("Daemon logs: {}", paths.logs_root.display());
            }
            Ok(())
        }
        DaemonCommand::Startup { command } => match command {
            StartupCommand::Install => {
                let daemon = sibling_binary("agenttracebackd").ok_or_else(|| {
                    CliError::DaemonUnavailable(
                        "agenttracebackd was not found next to this CLI.".to_owned(),
                    )
                })?;
                let status = install_startup(&daemon)?;
                print_startup_status(json, &status, true)
            }
            StartupCommand::Remove => {
                let status = remove_startup()?;
                print_startup_status(json, &status, false)
            }
            StartupCommand::Status => {
                let status = startup_status()?;
                print_startup_status(json, &status, false)
            }
        },
        DaemonCommand::Backup { output } => backup_database(&paths, output, json).await,
        DaemonCommand::Restore { input, confirm } => {
            restore_database(&paths, &input, confirm, json).await
        }
    }
}

fn print_startup_status(
    json: bool,
    status: &agenttraceback_platform::StartupStatus,
    changed: bool,
) -> Result<(), CliError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "installed": status.installed,
                "location": status.location,
                "changed": changed,
            }))?
        );
    } else if status.installed {
        println!("Startup registration is installed: {}", status.location);
    } else {
        println!("Startup registration is not installed: {}", status.location);
    }
    Ok(())
}

async fn backup_database(
    paths: &PlatformPaths,
    output: Option<PathBuf>,
    json: bool,
) -> Result<(), CliError> {
    if !paths.database_file.is_file() {
        return Err(CliError::DaemonUnavailable(
            "the local database does not exist".to_owned(),
        ));
    }
    let destination = output.unwrap_or_else(|| {
        paths
            .backups_root
            .join(format!("agenttraceback-backup-{}.db", current_time_us()))
    });
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if destination.exists() {
        return Err(CliError::DaemonUnavailable(format!(
            "backup destination already exists: {}",
            destination.display()
        )));
    }
    let database = paths.database_file.clone();
    let destination_for_backup = destination.clone();
    tokio::task::spawn_blocking(move || -> Result<(), CliError> {
        let connection = rusqlite::Connection::open(database)?;
        connection.busy_timeout(Duration::from_secs(30))?;
        let destination_text = destination_for_backup.to_string_lossy();
        connection.execute("VACUUM INTO ?1", [destination_text.as_ref()])?;
        set_private_file(&destination_for_backup)?;
        Ok(())
    })
    .await
    .map_err(|error| CliError::DaemonUnavailable(error.to_string()))??;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "backup": destination,
                "source": paths.database_file,
                "verifiedBy": "sqlite_vacuum_into",
            }))?
        );
    } else {
        println!("Database backup: {}", destination.display());
    }
    Ok(())
}

async fn restore_database(
    paths: &PlatformPaths,
    input: &Path,
    confirm: bool,
    json: bool,
) -> Result<(), CliError> {
    if !confirm {
        return Err(CliError::DaemonUnavailable(
            "restore requires --confirm".to_owned(),
        ));
    }
    if !input.is_file() {
        return Err(CliError::DaemonUnavailable(format!(
            "backup does not exist: {}",
            input.display()
        )));
    }
    if input == paths.database_file {
        return Err(CliError::DaemonUnavailable(
            "restore input must not be the active database".to_owned(),
        ));
    }
    let input_for_check = input.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), CliError> {
        let connection = rusqlite::Connection::open(input_for_check)?;
        let integrity: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(CliError::DaemonUnavailable(format!(
                "backup integrity check failed: {integrity}"
            )));
        }
        Ok(())
    })
    .await
    .map_err(|error| CliError::DaemonUnavailable(error.to_string()))??;

    let was_running = paths.read_runtime_metadata().is_ok();
    if was_running {
        stop_daemon(paths, false)?;
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    fs::create_dir_all(&paths.backups_root)?;
    let before_restore = paths.backups_root.join(format!(
        "agenttraceback-before-restore-{}.db",
        current_time_us()
    ));
    if paths.database_file.exists() {
        fs::copy(&paths.database_file, &before_restore)?;
    }
    fs::copy(input, &paths.database_file)?;
    for suffix in ["-wal", "-shm"] {
        let stale = PathBuf::from(format!("{}{suffix}", paths.database_file.display()));
        if stale.exists() {
            fs::remove_file(stale)?;
        }
    }
    set_private_file(&paths.database_file)?;
    if was_running {
        start_daemon(paths, false).await?;
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "restored": paths.database_file,
                "backup": input,
                "previousDatabaseBackup": before_restore,
                "daemonRunning": was_running,
            }))?
        );
    } else {
        println!("Restored database from {}", input.display());
        println!("Previous database backup: {}", before_restore.display());
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

async fn demo(json: bool, command: DemoCommand) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(120))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let path = match command {
        DemoCommand::Install => "/api/v1/demo/install",
        DemoCommand::Remove => "/api/v1/demo/remove",
    };
    let result: DemoDataView = post_json(
        &client,
        &base_url,
        path,
        &runtime.token,
        &serde_json::json!({}),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if result.installed {
        println!("{} installed.", result.label);
        println!("Project: {}", result.project_id);
        println!("Sessions: {}", result.sessions);
        println!("Events: {}", result.events);
    } else {
        println!("{} removed.", result.label);
        println!("Events removed: {}", result.events);
    }
    Ok(())
}

async fn start_daemon(paths: &PlatformPaths, json: bool) -> Result<(), CliError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    if let Ok(runtime) = paths.read_runtime_metadata() {
        let url = format!("http://127.0.0.1:{}/api/v1/health", runtime.port);
        if client
            .get(url)
            .bearer_auth(&runtime.token)
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            if json {
                println!("{}", serde_json::to_string_pretty(&runtime)?);
            } else {
                println!("Daemon already running (pid {}).", runtime.pid);
            }
            return Ok(());
        }
    }
    let daemon = sibling_binary("agenttracebackd").ok_or_else(|| {
        CliError::DaemonUnavailable(
            "agenttracebackd was not found next to this CLI. Set AGENTTRACEBACK_DAEMON_BIN."
                .to_owned(),
        )
    })?;
    let child = ProcessCommand::new(&daemon)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            CliError::DaemonUnavailable(format!("could not start {}: {error}", daemon.display()))
        })?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(runtime) = paths.read_runtime_metadata() {
            let url = format!("http://127.0.0.1:{}/api/v1/health", runtime.port);
            if client
                .get(url)
                .bearer_auth(&runtime.token)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                if json {
                    println!("{}", serde_json::to_string_pretty(&runtime)?);
                } else {
                    println!("Started daemon (pid {}).", runtime.pid);
                }
                return Ok(());
            }
        }
    }
    Err(CliError::DaemonUnavailable(format!(
        "daemon child {} did not become healthy",
        child.id()
    )))
}

fn stop_daemon(paths: &PlatformPaths, json: bool) -> Result<(), CliError> {
    let runtime = match paths.read_runtime_metadata() {
        Ok(runtime) => runtime,
        Err(_) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"stopped": false, "reason": "not_running"})
                );
            } else {
                println!("Daemon is not running.");
            }
            return Ok(());
        }
    };
    let status = if cfg!(windows) {
        ProcessCommand::new("taskkill")
            .args(["/PID", &runtime.pid.to_string(), "/T", "/F"])
            .status()
    } else {
        ProcessCommand::new("kill")
            .args(["-TERM", &runtime.pid.to_string()])
            .status()
    }
    .map_err(|error| CliError::DaemonUnavailable(error.to_string()))?;
    if !status.success() {
        return Err(CliError::DaemonUnavailable(format!(
            "could not stop daemon pid {}",
            runtime.pid
        )));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "stopped": true,
                "pid": runtime.pid,
            }))?
        );
    } else {
        println!("Stopped daemon (pid {}).", runtime.pid);
    }
    Ok(())
}

fn sibling_binary(name: &str) -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let candidate = executable.parent()?.join(name);
    candidate.is_file().then_some(candidate)
}

async fn adapters(json: bool, command: AdaptersCommand) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(300))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    match command {
        AdaptersCommand::List => {
            let response: AdapterScanResponse =
                get_json(&client, &base_url, "/api/v1/adapters", &runtime.token).await?;
            print_adapters(json, &response)?;
        }
        AdaptersCommand::Scan => {
            let response: AdapterScanResponse = post_json(
                &client,
                &base_url,
                "/api/v1/adapters/scan",
                &runtime.token,
                &serde_json::json!({}),
            )
            .await?;
            print_adapters(json, &response)?;
        }
        AdaptersCommand::Import { adapter_id } => {
            let response: AdapterImportResponse = post_json(
                &client,
                &base_url,
                &format!("/api/v1/adapters/{adapter_id}/import"),
                &runtime.token,
                &serde_json::json!({}),
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("Adapter: {}", response.adapter_id);
                println!("Sources: {}", response.sources);
                println!("Events imported: {}", response.events_imported);
                println!("Quarantined: {}", response.quarantined);
                for warning in response.warnings {
                    eprintln!("warning: {warning}");
                }
            }
        }
        AdaptersCommand::Hooks { command } => match command {
            HookCommand::Plan { adapter_id } => {
                let plan: Option<HookPlan> = post_json(
                    &client,
                    &base_url,
                    &format!("/api/v1/adapters/{adapter_id}/hooks/plan"),
                    &runtime.token,
                    &serde_json::json!({}),
                )
                .await?;
                if let Some(plan) = plan {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&plan)?);
                    } else {
                        println!("Adapter: {}", plan.adapter_id);
                        println!("Configuration: {}", plan.config_path.display());
                        println!("Plan digest: {}", plan.plan_digest);
                        println!("{}", plan.preview);
                    }
                } else if json {
                    println!("null");
                } else {
                    println!("No hook plan is available for {adapter_id}.");
                }
            }
            HookCommand::Install {
                adapter_id,
                approve,
            } => {
                let plan: Option<HookPlan> = post_json(
                    &client,
                    &base_url,
                    &format!("/api/v1/adapters/{adapter_id}/hooks/plan"),
                    &runtime.token,
                    &serde_json::json!({}),
                )
                .await?;
                let plan = plan.ok_or_else(|| CliError::DaemonHttp {
                    status: StatusCode::NOT_FOUND,
                    body: "No hook plan is available for this adapter.".to_owned(),
                })?;
                if plan.plan_digest != approve {
                    return Err(CliError::DaemonHttp {
                        status: StatusCode::CONFLICT,
                        body: format!(
                            "Approved digest does not match the current plan. Expected {}.",
                            plan.plan_digest
                        ),
                    });
                }
                let receipt: HookReceipt = post_json(
                    &client,
                    &base_url,
                    &format!("/api/v1/adapters/{adapter_id}/hooks/install"),
                    &runtime.token,
                    &InstallAdapterHookRequest {
                        plan,
                        idempotency_key: None,
                    },
                )
                .await?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&receipt)?);
                } else {
                    println!("Installed {} hook.", receipt.adapter_id);
                    println!("Backup: {}", receipt.backup_path.display());
                }
            }
            HookCommand::Uninstall { adapter_id } => {
                let _: serde_json::Value = post_json(
                    &client,
                    &base_url,
                    &format!("/api/v1/adapters/{adapter_id}/hooks/uninstall"),
                    &runtime.token,
                    &UninstallAdapterHookRequest {
                        idempotency_key: None,
                    },
                )
                .await?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "adapterId": adapter_id,
                            "uninstalled": true
                        }))?
                    );
                } else {
                    println!("Removed AgentTraceback-owned {adapter_id} hook entries.");
                }
            }
        },
    }
    Ok(())
}

fn print_adapters(json: bool, response: &AdapterScanResponse) -> Result<(), CliError> {
    if json {
        println!("{}", serde_json::to_string_pretty(response)?);
    } else if response.adapters.is_empty() {
        println!("No supported agents detected.");
    } else {
        for adapter in &response.adapters {
            println!(
                "{:<14} {:<20} {}",
                adapter.id,
                adapter.agent_version.as_deref().unwrap_or("unknown"),
                adapter.status
            );
            for root in &adapter.source_roots {
                println!("  source {root}");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionFileChange {
    path: String,
    before: Option<FileVersionRow>,
    after: Option<FileVersionRow>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileVersionRow {
    version_id: String,
    content_hash: Option<String>,
    byte_length: Option<u64>,
    capture_status: String,
    executable: bool,
    symlink_target: Option<String>,
}

async fn list_files(json: bool, session_id: &str) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(10))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let files: Vec<SessionFileChange> = get_json(
        &client,
        &base_url,
        &format!("/api/v1/sessions/{session_id}/files"),
        &runtime.token,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&files)?);
    } else if files.is_empty() {
        println!("No file versions recorded for this session.");
    } else {
        for file in files {
            let before = file
                .before
                .as_ref()
                .and_then(|version| version.content_hash.as_deref())
                .unwrap_or("none");
            let after = file
                .after
                .as_ref()
                .and_then(|version| version.content_hash.as_deref())
                .unwrap_or("none");
            println!("{}\n  before {}\n  after  {}", file.path, before, after);
        }
    }
    Ok(())
}

async fn export_session(
    json: bool,
    session_id: &str,
    format: &str,
    full: bool,
) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(300))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let response: ExportView = post_json(
        &client,
        &base_url,
        "/api/v1/exports",
        &runtime.token,
        &ExportRequest {
            session_id: session_id.to_owned(),
            format: format.to_owned(),
            full,
        },
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("Exported {} bundle.", response.format);
        println!("{}", response.path);
        println!("Redacted: {}", if response.redacted { "yes" } else { "no" });
    }
    Ok(())
}

async fn verify_chain(json: bool, session_id: Option<&str>, all: bool) -> Result<bool, CliError> {
    let (runtime, client) = local_client(Duration::from_secs(300))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let response = client
        .get(format!("{base_url}/api/v1/verify"))
        .bearer_auth(&runtime.token)
        .query(&session_id.map_or_else(
            || vec![("all".to_owned(), all.to_string())],
            |session_id| vec![("sessionId".to_owned(), session_id.to_owned())],
        ))
        .send()
        .await?;
    let report: VerificationView = decode_response(response).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.valid {
        println!("Chain verified: {}", report.scope);
    } else {
        println!("Integrity verification failed: {}", report.scope);
        println!(
            "missing={} reordered={} changed={} missing_payloads={} root_mismatch={}",
            report.missing_sequences.len(),
            report.reordered_sequences.len(),
            report.changed_entries.len(),
            report.missing_payloads.len(),
            report.root_mismatch
        );
    }
    Ok(report.valid)
}

async fn recover(json: bool, command: RecoverCommand) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(60))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    match command {
        RecoverCommand::Plan {
            session_id,
            action,
            destination,
            paths,
        } => {
            let plan: RecoveryPlanView = post_json(
                &client,
                &base_url,
                "/api/v1/recovery/plans",
                &runtime.token,
                &PlanRecoveryRequest {
                    session_id,
                    action,
                    destination: destination.to_string_lossy().into_owned(),
                    paths,
                },
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                println!("Recovery plan {}", plan.plan_id);
                println!("action: {}", plan.action);
                println!("destination: {}", plan.destination);
                println!("coverage: {}", plan.coverage);
                println!("restorable operations: {}", plan.operations.len());
                println!("exclusions: {}", plan.exclusions.len());
                println!("plan digest: {}", plan.plan_digest);
            }
        }
        RecoverCommand::Execute {
            plan_id,
            confirm,
            overwrite_conflicts,
        } => {
            let plan: RecoveryPlanView = get_json(
                &client,
                &base_url,
                &format!("/api/v1/recovery/plans/{plan_id}"),
                &runtime.token,
            )
            .await?;
            let run: RecoveryRunView = post_json(
                &client,
                &base_url,
                &format!("/api/v1/recovery/plans/{plan_id}/execute"),
                &runtime.token,
                &ExecuteRecoveryRequest {
                    plan_digest: plan.plan_digest,
                    confirm,
                    overwrite_conflicts,
                },
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&run)?);
            } else {
                println!("Recovery run {}", run.run_id);
                if let Some(backup_plan_id) = &run.backup_plan_id {
                    println!("Pre-restore backup plan: {backup_plan_id}");
                    println!(
                        "Undo with: agenttraceback recover execute {backup_plan_id} --confirm"
                    );
                }
                println!("state: {}", run.state);
                println!("destination: {}", run.destination);
                println!(
                    "restored: {}, skipped: {}, conflicts: {}",
                    run.restored_files, run.skipped_files, run.conflict_files
                );
            }
        }
    }
    Ok(())
}

async fn status(json: bool) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(5))?;
    let url = format!("http://127.0.0.1:{}/api/v1/health", runtime.port);
    let response = client
        .get(url)
        .bearer_auth(&runtime.token)
        .send()
        .await
        .map_err(|error| CliError::DaemonUnavailable(error.to_string()))?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(CliError::DaemonHttp { status, body });
    }
    let health: HealthResponse = serde_json::from_str(&body)?;
    let output = StatusOutput {
        reachable: true,
        api_version: API_VERSION,
        runtime: RuntimeView::from(&runtime),
        health: Some(health),
        error: None,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_human_status(&output);
    }
    Ok(())
}

fn print_human_status(output: &StatusOutput) {
    let Some(health) = &output.health else {
        println!("agenttracebackd: unavailable");
        return;
    };
    println!("agenttracebackd: {}", health.status);
    println!(
        "version: {} (API v{})",
        health.daemon_version, health.api_version
    );
    println!("endpoint: 127.0.0.1:{} (pid {})", health.port, health.pid);
    println!("database: {}", health.database.status);
    println!("capture: {}", health.capture.status);
    println!("key protection: {}", health.key_protection);
}

async fn search(json: bool, query: &str, limit: u16) -> Result<(), CliError> {
    let (runtime, client) = local_client(Duration::from_secs(5))?;
    let response = client
        .get(format!("http://127.0.0.1:{}/api/v1/search", runtime.port))
        .bearer_auth(&runtime.token)
        .query(&[("q", query.to_owned()), ("limit", limit.to_string())])
        .send()
        .await
        .map_err(|error| CliError::DaemonUnavailable(error.to_string()))?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(CliError::DaemonHttp { status, body });
    }
    let response: SearchResponse = serde_json::from_str(&body)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if response.items.is_empty() {
        println!("No matching events.");
    } else {
        for item in &response.items {
            let actor = item.agent.as_deref().unwrap_or("unattributed");
            let target = item
                .target_display
                .as_deref()
                .or(item.project_name.as_deref())
                .unwrap_or("-");
            println!(
                "{}  {:<16} {:<12} {}  [{}]",
                item.occurred_at_us, item.action, actor, target, item.evidence
            );
            if let Some(preview) = &item.redacted_preview {
                println!("  {preview}");
            }
            println!("  event {}", item.event_id);
        }
        if response.has_more {
            println!("More results are available; refine the query or raise --limit.");
        }
    }
    Ok(())
}

fn local_client(timeout: Duration) -> Result<(RuntimeMetadata, reqwest::Client), CliError> {
    let paths = PlatformPaths::discover()?;
    let runtime = paths.read_runtime_metadata().map_err(|error| {
        CliError::DaemonUnavailable(format!(
            "runtime metadata is unavailable ({}); start agenttracebackd",
            error
        ))
    })?;
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    Ok((runtime, client))
}

#[allow(clippy::too_many_arguments)]
async fn run_wrapper(
    json: bool,
    project: Option<PathBuf>,
    label: Option<String>,
    agent: Option<String>,
    no_transcript: bool,
    no_recovery_snapshot: bool,
    command: Vec<OsString>,
) -> Result<ExitCode, CliError> {
    let cwd = project.unwrap_or(env::current_dir()?);
    let Some(program) = command.first().cloned() else {
        return Ok(ExitCode::from(2));
    };
    let arguments = command.iter().skip(1).cloned().collect::<Vec<_>>();
    let command_preview = command
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let (runtime, client) = local_client(Duration::from_secs(60))?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let prepared: PrepareWrapperResponse = post_json(
        &client,
        &base_url,
        "/api/v1/wrapper/sessions/prepare",
        &runtime.token,
        &PrepareWrapperRequest {
            project_path: cwd.to_string_lossy().into_owned(),
            label,
            agent: agent.clone(),
            command_preview,
            no_recovery_snapshot,
        },
    )
    .await?;
    let session_id = prepared.session_id.clone();

    let (root_sender, root_receiver) = tokio::sync::oneshot::channel();
    let root_sender = Arc::new(std::sync::Mutex::new(Some(root_sender)));
    let root_client = client.clone();
    let root_base_url = base_url.clone();
    let root_token = runtime.token.clone();
    let root_session = session_id.clone();
    let root_task = tokio::spawn(async move {
        if let Ok(pid) = root_receiver.await {
            let _ = post_json::<serde_json::Value, _>(
                &root_client,
                &root_base_url,
                &format!("/api/v1/wrapper/sessions/{root_session}/root"),
                &root_token,
                &RegisterRootPidRequest { pid },
            )
            .await;
        }
    });

    let (transcript_sender, mut transcript_receiver) =
        tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let transcript_task = if no_transcript {
        None
    } else {
        let client = client.clone();
        let base_url = base_url.clone();
        let token = runtime.token.clone();
        let session_id = session_id.clone();
        Some(tokio::spawn(async move {
            while let Some(chunk) = transcript_receiver.recv().await {
                if post_bytes(
                    &client,
                    &base_url,
                    &format!("/api/v1/wrapper/sessions/{session_id}/transcript"),
                    &token,
                    chunk,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }))
    };

    let output_callback = if no_transcript {
        None
    } else {
        Some(Arc::new(move |chunk: Vec<u8>| {
            let _ = transcript_sender.send(chunk);
        }) as agenttraceback_platform::OutputCallback)
    };
    let spawn_callback = Some(Arc::new(move |pid| {
        if let Some(sender) = root_sender.lock().ok().and_then(|mut sender| sender.take()) {
            let _ = sender.send(pid);
        }
    }) as agenttraceback_platform::SpawnCallback);
    let options = PtyOptions {
        program,
        arguments,
        cwd,
        environment: vec![(
            OsString::from("AGENTTRACEBACK_SESSION_ID"),
            OsString::from(session_id.clone()),
        )],
        output_callback,
        spawn_callback,
    };
    let outcome = tokio::task::spawn_blocking(move || run_pty(options)).await;
    let _ = root_task.await;
    if let Some(task) = transcript_task {
        let _ = task.await;
    }
    let outcome = match outcome {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            let _ = complete_session(
                &client,
                &base_url,
                &runtime.token,
                &session_id,
                126,
                Some("launch_failed".to_owned()),
            )
            .await;
            return Err(CliError::Pty(error));
        }
        Err(error) => {
            let _ = complete_session(
                &client,
                &base_url,
                &runtime.token,
                &session_id,
                126,
                Some("launch_task_failed".to_owned()),
            )
            .await;
            return Err(CliError::DaemonUnavailable(error.to_string()));
        }
    };
    complete_session(
        &client,
        &base_url,
        &runtime.token,
        &session_id,
        outcome.exit_code,
        outcome.signal.clone(),
    )
    .await?;
    if !json {
        eprintln!("AgentTraceback recorded session {session_id}");
    }
    Ok(ExitCode::from(u8::try_from(outcome.exit_code).unwrap_or(1)))
}

async fn post_json<T, B>(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: &str,
    body: &B,
) -> Result<T, CliError>
where
    T: serde::de::DeserializeOwned,
    B: Serialize + ?Sized,
{
    let response = client
        .post(format!("{base_url}{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await?;
    decode_response(response).await
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: &str,
) -> Result<T, CliError> {
    let response = client
        .get(format!("{base_url}{path}"))
        .bearer_auth(token)
        .send()
        .await?;
    decode_response(response).await
}

async fn post_bytes(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: &str,
    body: Vec<u8>,
) -> Result<(), CliError> {
    let response = client
        .post(format!("{base_url}{path}"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(CliError::DaemonHttp {
            status,
            body: response.text().await?,
        })
    }
}

async fn decode_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, CliError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(CliError::DaemonHttp { status, body });
    }
    if body.trim().is_empty() {
        return serde_json::from_str("null").map_err(CliError::from);
    }
    serde_json::from_str(&body).map_err(CliError::from)
}

async fn complete_session(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    session_id: &str,
    exit_code: u32,
    signal: Option<String>,
) -> Result<(), CliError> {
    let response = client
        .post(format!(
            "{base_url}/api/v1/wrapper/sessions/{session_id}/complete"
        ))
        .bearer_auth(token)
        .json(&CompleteWrapperRequest {
            exit_code,
            signal,
            ended_at_us: current_time_us(),
        })
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(CliError::DaemonHttp {
            status,
            body: response.text().await?,
        });
    }
    Ok(())
}

fn current_time_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn version(json: bool) -> Result<(), CliError> {
    if json {
        let output = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "apiVersion": API_VERSION,
            "eventSchemaVersion": agenttraceback_types::EVENT_SCHEMA_VERSION,
            "platform": env::consts::OS,
            "architecture": env::consts::ARCH,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("agenttraceback {}", env!("CARGO_PKG_VERSION"));
        println!("local API v{API_VERSION}");
        println!(
            "event schema v{}",
            agenttraceback_types::EVENT_SCHEMA_VERSION
        );
        println!("{}-{}", env::consts::OS, env::consts::ARCH);
    }
    Ok(())
}
