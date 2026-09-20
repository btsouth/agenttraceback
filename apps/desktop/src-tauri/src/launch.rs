//! Launch only supported agents, with literal arguments, in a system terminal.
use serde::Serialize;
#[cfg(unix)]
use std::process::Stdio;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchResult {
    terminal: String,
}

fn agent_program(agent: &str) -> Result<&'static str, String> {
    match agent {
        "codex" => Ok("codex"),
        "claude-code" => Ok("claude"),
        "gemini-cli" => Ok("gemini"),
        "opencode" => Ok("opencode"),
        "hermes" => Ok("hermes"),
        _ => Err("Choose a supported agent.".into()),
    }
}

fn executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        // Never resolve programs from the selected project or a relative PATH entry.
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| {
            #[cfg(windows)]
            let names = vec![dir.join(format!("{name}.exe")), dir.join(format!("{name}.cmd")), dir.join(format!("{name}.bat"))];
            #[cfg(not(windows))]
            let names = vec![dir.join(name)];
            names
        })
        .find(|path| executable(path))
}

fn run_args(project: &Path, agent: &str, program: &Path, transcript: bool) -> Vec<OsString> {
    let mut args = vec![
        "run".into(),
        "--project".into(),
        project.as_os_str().into(),
        "--agent".into(),
        agent.into(),
    ];
    if !transcript {
        args.push("--no-transcript".into());
    }
    args.extend(["--".into(), program.as_os_str().into()]);
    args
}

fn clean_terminal_env(command: &mut Command) {
    // AppImage's bundled GTK/Wayland libraries must not leak into the user's terminal.
    for key in [
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "APPDIR",
        "APPIMAGE",
        "GTK_PATH",
        "GTK_EXE_PREFIX",
        "GTK_DATA_PREFIX",
        "GTK_IM_MODULE_FILE",
        "GDK_PIXBUF_MODULE_FILE",
        "GIO_EXTRA_MODULES",
        "GI_TYPELIB_PATH",
        "GSETTINGS_SCHEMA_DIR",
    ] {
        command.env_remove(key);
    }
}

#[cfg(target_os = "linux")]
fn terminal_command(cli: &Path, args: &[OsString]) -> Result<(Command, String), String> {
    linux_terminal_command(cli, args, on_path)
}

#[cfg(target_os = "linux")]
fn linux_terminal_command(
    cli: &Path,
    args: &[OsString],
    lookup: impl Fn(&str) -> Option<PathBuf>,
) -> Result<(Command, String), String> {
    for (name, prefix) in [
        ("xdg-terminal-exec", &["--"][..]),
        ("ghostty", &["-e"][..]),
        ("kitty", &[][..]),
        ("alacritty", &["-e"][..]),
        ("gnome-terminal", &["--"][..]),
        ("konsole", &["-e"][..]),
        ("wezterm", &["start", "--"][..]),
        ("xterm", &["-e"][..]),
    ] {
        if let Some(binary) = lookup(name) {
            let mut command = Command::new(binary);
            command.args(prefix).arg(cli).args(args);
            return Ok((command, name.into()));
        }
    }
    Err(
        "No supported terminal found. Install a terminal and xdg-terminal-exec, then try again."
            .into(),
    )
}

#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn terminal_command(cli: &Path, args: &[OsString]) -> Result<(Command, String), String> {
    // AppleScript receives the shell command as an argv value, never as script source.
    let line = std::iter::once(cli.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|part| shell_quote(&part.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = Command::new("/usr/bin/osascript");
    command
        .args([
            "-e",
            "on run argv",
            "-e",
            "tell application \"Terminal\"",
            "-e",
            "activate",
            "-e",
            "do script (item 1 of argv)",
            "-e",
            "end tell",
            "-e",
            "end run",
            "--",
        ])
        .arg(line);
    Ok((command, "Terminal".into()))
}

#[cfg(windows)]
fn terminal_command(cli: &Path, args: &[OsString]) -> Result<(Command, String), String> {
    use std::os::windows::process::CommandExt;
    let quote =
        |value: &std::ffi::OsStr| format!("'{}'", value.to_string_lossy().replace('\'', "''"));
    let line = format!(
        "& {}",
        std::iter::once(cli.as_os_str())
            .chain(args.iter().map(OsString::as_os_str))
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NoExit", "-Command"])
        .arg(line)
        .creation_flags(0x00000010);
    Ok((command, "PowerShell".into()))
}

pub(crate) fn launch(
    project: String,
    agent: String,
    transcript: bool,
) -> Result<LaunchResult, String> {
    let program_name = agent_program(&agent)?;
    let input = Path::new(&project);
    if !input.is_absolute() {
        return Err("Choose an absolute project folder path using Browse.".into());
    }
    let project = input
        .canonicalize()
        .map_err(|e| format!("Cannot open project folder: {e}"))?;
    if !project.is_dir() {
        return Err("The project path must be a folder.".into());
    }
    let program = on_path(program_name).ok_or_else(|| format!("{program_name} is not installed or is missing from PATH. Install the agent, then reopen AgentTraceback."))?;
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let cli = current.with_file_name(if cfg!(windows) {
        "agenttraceback.exe"
    } else {
        "agenttraceback"
    });
    if !executable(&cli) {
        return Err("The bundled recording CLI is missing. Reinstall the desktop package.".into());
    }
    let (mut command, terminal) =
        terminal_command(&cli, &run_args(&project, &agent, &program, transcript))?;
    command.current_dir(&project);
    clean_terminal_env(&mut command);
    #[cfg(unix)]
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not open {terminal}: {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    match child.try_wait().map_err(|e| e.to_string())? {
        Some(status) if !status.success() => {
            return Err(format!(
                "{terminal} could not launch the recorded run ({status}). Check your terminal configuration."
            ));
        }
        Some(_) => {}
        None => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    Ok(LaunchResult { terminal })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_arguments_preserve_shell_metacharacters_as_literals() {
        let project = Path::new("/tmp/project space ' $(touch nope); --help");
        let args = run_args(project, "codex", Path::new("/bin/codex"), false);
        assert_eq!(args[2], project.as_os_str());
        assert_eq!(args[5], "--no-transcript");
        assert_eq!(args[6], "--");
        assert_eq!(args.len(), 8);
        assert!(
            !run_args(project, "codex", Path::new("/bin/codex"), true)
                .contains(&OsString::from("--no-transcript"))
        );
    }
    #[test]
    fn invalid_inputs_never_launch() {
        assert!(agent_program("codex; touch nope").is_err());
        assert!(launch("relative/path".into(), "codex".into(), true).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_process_gets_literal_arguments_and_clean_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("terminal");
        let output = dir.path().join("args");
        std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$LAUNCH_TEST_OUTPUT\"\ntest -z \"$LD_LIBRARY_PATH$LD_PRELOAD$APPDIR$APPIMAGE\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = Path::new("/tmp/spaces ' $(touch nope)");
        let args = run_args(project, "codex", Path::new("/agents/codex"), false);
        let (mut command, name) =
            linux_terminal_command(Path::new("/bundled/agenttraceback"), &args, |name| {
                (name == "xdg-terminal-exec").then(|| fake.clone())
            })
            .unwrap();
        assert_eq!(name, "xdg-terminal-exec");
        command
            .env("LAUNCH_TEST_OUTPUT", &output)
            .env("LD_LIBRARY_PATH", "/bad-libraries")
            .env("APPDIR", "/bad-appdir");
        clean_terminal_env(&mut command);
        assert!(command.status().unwrap().success());
        let actual = std::fs::read_to_string(output).unwrap();
        assert_eq!(
            actual.lines().collect::<Vec<_>>(),
            vec![
                "--",
                "/bundled/agenttraceback",
                "run",
                "--project",
                project.to_str().unwrap(),
                "--agent",
                "codex",
                "--no-transcript",
                "--",
                "/agents/codex"
            ]
        );
        assert!(linux_terminal_command(Path::new("/cli"), &[], |_| None).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quotes_shell_strings() {
        assert_eq!(shell_quote("a'b$(x)"), "'a'\\''b$(x)'");
    }
}
