use std::{fs, io::Write, path::PathBuf, process::ExitCode};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(about = "Deterministic fixture agent for AgentTraceback recorder tests")]
struct Args {
    /// Scenario to execute.
    #[arg(long, value_enum, default_value_t = Scenario::Basic)]
    scenario: Scenario,
    /// Directory where scenario files are created.
    #[arg(long, default_value = ".")]
    output: PathBuf,
    /// Optional sensitive-path probe.
    #[arg(long)]
    sensitive_path: Option<PathBuf>,
    /// Emit one intentionally malformed JSON frame before normal output.
    #[arg(long)]
    malformed: bool,
    /// Hold the session active for this many milliseconds before mutation.
    #[arg(long, default_value_t = 0)]
    hold_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Scenario {
    Basic,
    Child,
    ChildLeaf,
    Failure,
    Sensitive,
    Crash,
    Concurrent,
    Recovery,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureEvent<'a> {
    timestamp: String,
    kind: &'a str,
    text: &'a str,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("fixture-agent: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8> {
    let args = Args::parse();
    fs::create_dir_all(&args.output)
        .with_context(|| format!("could not create {}", args.output.display()))?;

    if args.malformed {
        println!("{{not-valid-json");
    }

    emit("prompt", "fixture prompt: exercise the recorder")?;
    if args.hold_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(args.hold_ms));
    }
    match args.scenario {
        Scenario::Basic => basic(&args.output)?,
        Scenario::Child => child(&args.output)?,
        Scenario::ChildLeaf => child_leaf(&args.output)?,
        Scenario::Failure => return failure(&args.output),
        Scenario::Sensitive => sensitive(&args.output, args.sensitive_path.as_ref())?,
        Scenario::Crash => {
            emit("tool_call", "simulate a recorder-visible crash")?;
            return Ok(86);
        }
        Scenario::Concurrent => concurrent(&args.output)?,
        Scenario::Recovery => recovery(&args.output)?,
    }
    emit("response", "fixture scenario complete")?;
    Ok(0)
}

fn basic(output: &std::path::Path) -> Result<()> {
    emit("tool_call", "write src/example.txt")?;
    let source_dir = output.join("src");
    fs::create_dir_all(&source_dir)
        .with_context(|| format!("could not create {}", source_dir.display()))?;
    fs::write(source_dir.join("example.txt"), "fixture v1\n")?;
    fs::rename(
        source_dir.join("example.txt"),
        source_dir.join("renamed.txt"),
    )?;
    fs::write(source_dir.join("delete-me.txt"), "fixture delete\n")?;
    fs::remove_file(source_dir.join("delete-me.txt"))?;
    emit("tool_result", "file create, write, and delete complete")?;
    Ok(())
}

fn child(output: &std::path::Path) -> Result<()> {
    emit("tool_call", "spawn child and grandchild processes")?;
    let executable = std::env::current_exe().context("could not resolve fixture-agent path")?;
    let status = std::process::Command::new(executable)
        .args([
            "--scenario",
            "child-leaf",
            "--output",
            &output.to_string_lossy(),
        ])
        .status()
        .context("could not spawn fixture child")?;
    if !status.success() {
        bail!("fixture child failed with {status}");
    }
    emit("tool_result", "child process complete")?;
    Ok(())
}

fn child_leaf(output: &std::path::Path) -> Result<()> {
    emit("subagent_start", "grandchild fixture started")?;
    let path = output.join("child-output.txt");
    fs::write(&path, "child process\n")?;
    emit("subagent_end", "grandchild fixture completed")?;
    Ok(())
}

fn failure(output: &std::path::Path) -> Result<u8> {
    emit("test_start", "fixture failing test")?;
    fs::write(output.join("test-output.txt"), "FAILED\n")?;
    emit("test_result", "fixture test failed")?;
    Ok(1)
}

fn sensitive(output: &std::path::Path, sensitive_path: Option<&PathBuf>) -> Result<()> {
    let path = sensitive_path
        .cloned()
        .unwrap_or_else(|| output.join(".env.fixture"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        "FIXTURE_API_KEY=sk-agenttraceback-not-a-real-secret-0000\n",
    )?;
    emit("file_read", "read fixture sensitive path")?;
    emit("file_write", "wrote synthetic sensitive fixture")?;
    Ok(())
}

fn concurrent(output: &std::path::Path) -> Result<()> {
    emit("tool_call", "concurrent fixture write")?;
    let file = output.join("concurrent.txt");
    let mut handle = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    writeln!(handle, "fixture concurrent record")?;
    emit("tool_result", "concurrent fixture write complete")?;
    Ok(())
}

fn recovery(output: &std::path::Path) -> Result<()> {
    emit("tool_call", "exercise recovery scenario")?;
    fs::write(output.join("dirty.txt"), "changed during session\n")?;
    fs::remove_file(output.join("untracked.txt"))?;
    fs::write(output.join("created-during-session.txt"), "new file\n")?;
    emit("tool_result", "recovery fixture mutations complete")?;
    Ok(())
}

fn emit(kind: &str, text: &str) -> Result<()> {
    let event = FixtureEvent {
        timestamp: Utc::now().to_rfc3339(),
        kind,
        text,
    };
    println!("{}", serde_json::to_string(&event)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::basic;

    #[test]
    fn basic_scenario_creates_expected_final_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        basic(directory.path()).expect("basic scenario");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("src/renamed.txt"))
                .expect("renamed fixture"),
            "fixture v1\n"
        );
        assert!(!directory.path().join("src/example.txt").exists());
        assert!(!directory.path().join("src/delete-me.txt").exists());
    }
}
