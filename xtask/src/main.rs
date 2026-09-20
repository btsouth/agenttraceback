use std::{
    env,
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

const SIDECAR_BUNDLE_CONFIG: &str =
    "{\"bundle\":{\"externalBin\":[\"binaries/agenttracebackd\",\"binaries/agenttraceback\"]}}";

#[derive(Debug, Parser)]
#[command(about = "AgentTraceback repository automation")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Validate developer prerequisites and repository layout.
    Doctor,
    /// Run the same formatting, lint, test, and build gates used by CI.
    Ci,
    /// Build release binaries and the desktop frontend.
    Package,
    /// Build and stage CLI/daemon sidecars for Tauri dev and packaging.
    Sidecars,
    /// Run the million-event storage and search acceptance benchmark.
    Benchmark {
        /// Number of events to generate.
        #[arg(long, default_value_t = 1_000_000)]
        events: usize,
    },
    /// Validate the checked OpenAPI contract.
    Openapi {
        /// Fail if the document is missing or lacks required routes.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Task::Doctor => doctor(),
        Task::Ci => ci(),
        Task::Package => package(),
        Task::Sidecars => prepare_sidecars(None),
        Task::Benchmark { events } => benchmark(events),
        Task::Openapi { check } => openapi(check),
    }
}

fn doctor() -> Result<()> {
    let checks = [
        ("rustc", &["--version"][..], true),
        ("cargo", &["--version"][..], true),
        ("git", &["--version"][..], true),
        ("node", &["--version"][..], true),
        ("pnpm", &["--version"][..], true),
    ];
    let mut failed = false;
    for (program, arguments, required) in checks {
        match capture(program, arguments) {
            Ok(output) => println!("{program:>8}: {}", output.trim()),
            Err(error) => {
                println!("{program:>8}: unavailable ({error})");
                failed |= required;
            }
        }
    }

    for directory in [
        "crates",
        "apps/desktop",
        "fixtures/fixture-agent",
        "migrations",
        "docs/adr",
    ] {
        if !Path::new(directory).is_dir() {
            println!("layout: missing {directory}");
            failed = true;
        }
    }

    if failed {
        bail!("doctor found missing required prerequisites");
    }
    println!("doctor: healthy");
    Ok(())
}

fn ci() -> Result<()> {
    run_command("cargo", ["fmt", "--all", "--check"])?;
    run_command(
        "cargo",
        [
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run_command("cargo", ["test", "--workspace"])?;
    pnpm(["install", "--frozen-lockfile"])?;
    pnpm(["lint"])?;
    pnpm(["typecheck"])?;
    pnpm(["test"])?;
    pnpm(["build"])?;
    pnpm(["--filter", "@agenttraceback/desktop", "test:e2e"])?;
    if cfg!(unix) {
        run_command("bash", ["./scripts/release-smoke.sh"])?;
    }
    if capture("cargo-deny", &["--version"]).is_ok() {
        run_command("cargo", ["deny", "check"])?;
    } else {
        println!("cargo-deny: not installed; advisory/license policy check skipped");
    }
    openapi(true)?;
    Ok(())
}

fn openapi(check: bool) -> Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask manifest has no parent"))?
        .join("docs/openapi.json");
    let document: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("could not read {}", path.display()))?,
    )
    .with_context(|| format!("could not parse {}", path.display()))?;
    if !check {
        println!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(());
    }
    if document["openapi"] != "3.1.0" {
        bail!("OpenAPI document must use version 3.1.0");
    }
    let paths = document["paths"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("OpenAPI paths object is missing"))?;
    for required in [
        "/api/v1/health",
        "/api/v1/capabilities",
        "/api/v1/dashboard",
        "/api/v1/projects",
        "/api/v1/sessions",
        "/api/v1/sessions/{id}/events",
        "/api/v1/files",
        "/api/v1/findings",
        "/api/v1/search",
        "/api/v1/adapters",
        "/api/v1/adapters/{id}/hooks/plan",
        "/api/v1/adapters/{id}/hooks/install",
        "/api/v1/adapters/{id}/hooks/uninstall",
        "/api/v1/recovery/plans",
        "/api/v1/exports",
        "/api/v1/verify",
        "/api/v1/demo/install",
        "/api/v1/demo/remove",
    ] {
        if !paths.contains_key(required) {
            bail!("OpenAPI document is missing {required}");
        }
    }
    println!("openapi: {} routes validated", paths.len());
    Ok(())
}

fn package() -> Result<()> {
    prepare_sidecars(None)?;
    pnpm(["build"])?;
    if cfg!(target_os = "linux") {
        let full = run_os_command(
            program("pnpm"),
            [
                "--filter",
                "@agenttraceback/desktop",
                "tauri",
                "build",
                "--bundles",
                "deb,rpm,appimage",
                "--config",
                SIDECAR_BUNDLE_CONFIG,
            ],
        );
        if full.is_err() {
            eprintln!(
                "xtask: AppImage bundling failed on this host; retrying with deb and rpm only"
            );
            run_os_command(
                program("pnpm"),
                [
                    "--filter",
                    "@agenttraceback/desktop",
                    "tauri",
                    "build",
                    "--bundles",
                    "deb,rpm",
                    "--config",
                    SIDECAR_BUNDLE_CONFIG,
                ],
            )?;
        }
    } else {
        run_os_command(
            program("pnpm"),
            [
                "--filter",
                "@agenttraceback/desktop",
                "tauri",
                "build",
                "--config",
                SIDECAR_BUNDLE_CONFIG,
            ],
        )?;
    }
    Ok(())
}

fn prepare_sidecars(target: Option<&str>) -> Result<()> {
    let mut arguments = vec![
        "build",
        "--release",
        "-p",
        "agenttraceback-daemon",
        "-p",
        "agenttraceback-cli",
    ];
    if let Some(target) = target {
        arguments.extend(["--target", target]);
    }
    run_command("cargo", arguments)?;
    let host = capture("rustc", &["-vV"])?
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::trim)
        .ok_or_else(|| anyhow::anyhow!("rustc did not report a host target"))?
        .to_owned();
    let target = target.unwrap_or(&host);
    let release = if target == host.as_str() {
        Path::new("target/release").to_path_buf()
    } else {
        Path::new("target").join(target).join("release")
    };
    let destination = Path::new("apps/desktop/src-tauri/binaries");
    fs::create_dir_all(destination)?;
    for name in ["agenttraceback", "agenttracebackd"] {
        let executable = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        let sidecar = if cfg!(windows) {
            format!("{name}-{target}.exe")
        } else {
            format!("{name}-{target}")
        };
        fs::copy(release.join(&executable), destination.join(sidecar))?;
    }
    Ok(())
}

fn benchmark(events: usize) -> Result<()> {
    println!(
        "$ AGENTTRACEBACK_BENCH_EVENTS={events} cargo test --release -p agenttraceback-store ..."
    );
    let status = Command::new("cargo")
        .args([
            "test",
            "--release",
            "-p",
            "agenttraceback-store",
            "stores_one_million_events_and_searches_under_budget",
            "--",
            "--ignored",
            "--nocapture",
        ])
        .env("AGENTTRACEBACK_BENCH_EVENTS", events.to_string())
        .status()
        .context("could not start storage benchmark")?;
    if status.success() {
        Ok(())
    } else {
        bail!("storage benchmark exited with {status}")
    }
}

fn pnpm<I, S>(arguments: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_os_command(program("pnpm"), arguments)
}

fn run_command<I, S>(program: &str, arguments: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_os_command(program, arguments)
}

fn run_os_command<I, S>(program: impl AsRef<OsStr>, arguments: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let arguments: Vec<_> = arguments.into_iter().collect();
    println!(
        "$ {} {}",
        program.to_string_lossy(),
        render_arguments(&arguments)
    );
    let status = Command::new(program)
        .args(arguments.iter().map(AsRef::as_ref))
        .status()
        .with_context(|| format!("could not start {}", program.to_string_lossy()))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{} exited with {status}", program.to_string_lossy())
    }
}

fn capture(program: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .with_context(|| format!("could not execute {program}"))?;
    if !output.status.success() {
        bail!("{program} exited with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn render_arguments<S: AsRef<OsStr>>(arguments: &[S]) -> String {
    arguments
        .iter()
        .map(|argument| argument.as_ref().to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn program(name: &str) -> &str {
    if cfg!(windows) {
        match name {
            "pnpm" => "pnpm.cmd",
            _ => name,
        }
    } else {
        name
    }
}

#[allow(dead_code)]
fn workspace_root() -> Result<&'static Path> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask manifest has no parent"))
}
