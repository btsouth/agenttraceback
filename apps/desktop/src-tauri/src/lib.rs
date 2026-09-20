//! Tauri bridge for the AgentTraceback desktop application.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use agenttraceback_api::{
    AdapterImportResponse, AdapterScanResponse, CapabilitiesResponse, DashboardView,
    ExecuteRecoveryRequest, ExportRequest, ExportView, FileSummary, FindingSummary, HealthResponse,
    PlanRecoveryRequest, RecoveryPlanView, RecoveryRunView, SearchResponse, SessionFileChange,
    SessionView,
};
use agenttraceback_config::PlatformPaths;
use agenttraceback_types::{EventEnvelope, RuntimeMetadata};
use serde::Serialize;
use tauri::State;

#[derive(Clone)]
struct DesktopState {
    paths: PlatformPaths,
    client: reqwest::Client,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSnapshot {
    health: HealthResponse,
    capabilities: CapabilitiesResponse,
}

#[tauri::command]
async fn daemon_snapshot(state: State<'_, DesktopState>) -> Result<DesktopSnapshot, String> {
    let runtime = ensure_daemon(&state.paths, &state.client).await?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    let health =
        fetch_json::<HealthResponse>(&state.client, &base_url, "/api/v1/health", &runtime.token)
            .await?;
    let capabilities = fetch_json::<CapabilitiesResponse>(
        &state.client,
        &base_url,
        "/api/v1/capabilities",
        &runtime.token,
    )
    .await?;
    Ok(DesktopSnapshot {
        health,
        capabilities,
    })
}

#[tauri::command]
async fn dashboard(state: State<'_, DesktopState>) -> Result<DashboardView, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(
        &state.client,
        &base_url,
        "/api/v1/dashboard",
        &runtime.token,
    )
    .await
}

#[tauri::command]
async fn sessions(state: State<'_, DesktopState>) -> Result<Vec<SessionView>, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(
        &state.client,
        &base_url,
        "/api/v1/sessions?limit=200",
        &runtime.token,
    )
    .await
}

#[tauri::command]
async fn session(
    state: State<'_, DesktopState>,
    session_id: String,
) -> Result<SessionView, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(
        &state.client,
        &base_url,
        &format!("/api/v1/sessions/{session_id}"),
        &runtime.token,
    )
    .await
}

#[tauri::command]
async fn session_events(
    state: State<'_, DesktopState>,
    session_id: String,
) -> Result<Vec<EventEnvelope>, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    let mut events = Vec::new();
    loop {
        let page: Vec<EventEnvelope> = get_json(
            &state.client,
            &base_url,
            &format!(
                "/api/v1/sessions/{session_id}/events?limit=500&offset={}",
                events.len()
            ),
            &runtime.token,
        )
        .await?;
        let complete = page.len() < 500;
        events.extend(page);
        if complete || events.len() >= 100_000 {
            break;
        }
    }
    Ok(events)
}

#[tauri::command]
async fn session_files(
    state: State<'_, DesktopState>,
    session_id: String,
) -> Result<Vec<SessionFileChange>, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(
        &state.client,
        &base_url,
        &format!("/api/v1/sessions/{session_id}/files"),
        &runtime.token,
    )
    .await
}

#[tauri::command]
async fn files(state: State<'_, DesktopState>) -> Result<Vec<FileSummary>, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(
        &state.client,
        &base_url,
        "/api/v1/files?limit=200",
        &runtime.token,
    )
    .await
}

#[tauri::command]
async fn findings(state: State<'_, DesktopState>) -> Result<Vec<FindingSummary>, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(
        &state.client,
        &base_url,
        "/api/v1/findings?limit=200",
        &runtime.token,
    )
    .await
}

#[tauri::command]
async fn export_session(
    state: State<'_, DesktopState>,
    session_id: String,
    format: String,
    full: Option<bool>,
) -> Result<ExportView, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    post_json(
        &state.client,
        &base_url,
        "/api/v1/exports",
        &runtime.token,
        &ExportRequest {
            session_id,
            format,
            full: full.unwrap_or(false),
        },
    )
    .await
}

#[tauri::command]
async fn search_history(
    state: State<'_, DesktopState>,
    query: String,
    limit: Option<u16>,
) -> Result<SearchResponse, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    let response = state
        .client
        .get(format!("{base_url}/api/v1/search"))
        .bearer_auth(&runtime.token)
        .query(&[("q", query), ("limit", limit.unwrap_or(100).to_string())])
        .send()
        .await
        .map_err(|error| format!("local API request failed: {error}"))?;
    decode_response(response).await
}

#[tauri::command]
async fn adapters(state: State<'_, DesktopState>) -> Result<AdapterScanResponse, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    get_json(&state.client, &base_url, "/api/v1/adapters", &runtime.token).await
}

#[tauri::command]
async fn import_adapter(
    state: State<'_, DesktopState>,
    adapter_id: String,
) -> Result<AdapterImportResponse, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    post_json(
        &state.client,
        &base_url,
        &format!("/api/v1/adapters/{adapter_id}/import"),
        &runtime.token,
        &serde_json::json!({}),
    )
    .await
}

#[tauri::command]
async fn plan_recovery(
    state: State<'_, DesktopState>,
    request: PlanRecoveryRequest,
) -> Result<RecoveryPlanView, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    post_json(
        &state.client,
        &base_url,
        "/api/v1/recovery/plans",
        &runtime.token,
        &request,
    )
    .await
}

#[tauri::command]
async fn execute_recovery(
    state: State<'_, DesktopState>,
    plan_id: String,
    request: ExecuteRecoveryRequest,
) -> Result<RecoveryRunView, String> {
    let (runtime, base_url) = daemon_endpoint(&state).await?;
    post_json(
        &state.client,
        &base_url,
        &format!("/api/v1/recovery/plans/{plan_id}/execute"),
        &runtime.token,
        &request,
    )
    .await
}

async fn daemon_endpoint(state: &DesktopState) -> Result<(RuntimeMetadata, String), String> {
    let runtime = ensure_daemon(&state.paths, &state.client).await?;
    let base_url = format!("http://127.0.0.1:{}", runtime.port);
    Ok((runtime, base_url))
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: &str,
) -> Result<T, String> {
    let response = client
        .get(format!("{base_url}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| format!("local API request failed: {error}"))?;
    decode_response(response).await
}

async fn post_json<T, B>(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: &str,
    body: &B,
) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
    B: Serialize + ?Sized,
{
    let response = client
        .post(format!("{base_url}{path}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("local API request failed: {error}"))?;
    decode_response(response).await
}

async fn decode_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("could not read local API response: {error}"))?;
    if !status.is_success() {
        return Err(format!("local API returned HTTP {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|error| format!("invalid local API response: {error}"))
}

async fn ensure_daemon(
    paths: &PlatformPaths,
    client: &reqwest::Client,
) -> Result<RuntimeMetadata, String> {
    if let Ok(runtime) = paths.read_runtime_metadata()
        && health_reachable(client, &runtime).await
    {
        return Ok(runtime);
    }

    let daemon_binary = find_daemon_binary()?;
    Command::new(&daemon_binary)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            format!(
                "could not start {}: {error}",
                daemon_binary.to_string_lossy()
            )
        })?;

    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(runtime) = paths.read_runtime_metadata()
            && health_reachable(client, &runtime).await
        {
            return Ok(runtime);
        }
    }
    Err("agenttracebackd did not become healthy within four seconds".to_owned())
}

async fn health_reachable(client: &reqwest::Client, runtime: &RuntimeMetadata) -> bool {
    let url = format!("http://127.0.0.1:{}/api/v1/health", runtime.port);
    client
        .get(url)
        .bearer_auth(&runtime.token)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    token: &str,
) -> Result<T, String> {
    let response = client
        .get(format!("{base_url}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| format!("local API request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("could not read local API response: {error}"))?;
    if !status.is_success() {
        return Err(format!("local API returned HTTP {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|error| format!("invalid local API response: {error}"))
}

fn find_daemon_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("AGENTTRACEBACK_DAEMON_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "AGENTTRACEBACK_DAEMON_BIN does not point to a file: {}",
            path.display()
        ));
    }

    let current = std::env::current_exe()
        .map_err(|error| format!("could not resolve desktop executable: {error}"))?;
    let candidate = sibling_daemon_path(&current);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!(
            "agenttracebackd was not found next to the desktop executable; build it with `cargo build -p agenttraceback-daemon` or set AGENTTRACEBACK_DAEMON_BIN (looked for {})",
            candidate.display()
        ))
    }
}

fn sibling_daemon_path(current: &Path) -> PathBuf {
    let binary_name = if cfg!(windows) {
        "agenttracebackd.exe"
    } else {
        "agenttracebackd"
    };
    current
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(binary_name)
}

// Unsigned alpha builds omit updater configuration entirely. Registering the
// plugin in those builds fails during initialization before a window can open.
fn updater_is_configured(config: &tauri::Config) -> bool {
    config
        .plugins
        .0
        .get("updater")
        .is_some_and(|value| !value.is_null())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = PlatformPaths::discover().expect("platform paths");
    paths.ensure_directories().expect("application directories");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("local API client");
    let state = DesktopState { paths, client };

    let context = tauri::generate_context!();
    let mut builder = tauri::Builder::default();
    if updater_is_configured(context.config()) {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    builder
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            daemon_snapshot,
            dashboard,
            sessions,
            session,
            session_events,
            session_files,
            files,
            findings,
            export_session,
            search_history,
            adapters,
            import_adapter,
            plan_recovery,
            execute_recovery,
        ])
        .run(context)
        .expect("error while running AgentTraceback desktop application");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{sibling_daemon_path, updater_is_configured};

    #[test]
    fn updater_is_optional_for_unsigned_desktop_builds() {
        let mut config: tauri::Config = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("shipping desktop configuration");
        assert!(!updater_is_configured(&config));
        config
            .plugins
            .0
            .insert("updater".into(), serde_json::Value::Null);
        assert!(!updater_is_configured(&config));
        config.plugins.0.insert(
            "updater".into(),
            serde_json::json!({
                "pubkey": "configured-by-release-workflow",
                "endpoints": ["https://example.com/latest.json"]
            }),
        );
        assert!(updater_is_configured(&config));
    }

    #[test]
    fn daemon_binary_is_resolved_as_a_sibling() {
        let current = if cfg!(windows) {
            Path::new("target/debug/agenttraceback-desktop.exe")
        } else {
            Path::new("target/debug/agenttraceback-desktop")
        };
        let expected = if cfg!(windows) {
            "target/debug/agenttracebackd.exe"
        } else {
            "target/debug/agenttracebackd"
        };
        assert_eq!(sibling_daemon_path(current), Path::new(expected));
    }
}
