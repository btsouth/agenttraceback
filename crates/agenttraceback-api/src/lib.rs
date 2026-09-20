//! Authenticated HTTP and WebSocket surface bound only to loopback.

use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};

use agenttraceback_adapter_sdk::{HookPlan, HookReceipt};
use agenttraceback_blobs::BlobStore;
use agenttraceback_export::{
    EvidenceBundle, ExportContent, ExportFileChange, ExportFinding, ExportSession, render_json,
    render_markdown,
};
use agenttraceback_search::SearchQuery;
use agenttraceback_store::Store;
use agenttraceback_types::{API_VERSION, ApiErrorBody, ApiErrorDetail, EntityId};
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Path, Query, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tower_http::{
    limit::RequestBodyLimitLayer, set_header::SetResponseHeaderLayer, trace::TraceLayer,
};

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_QUERY_BYTES: usize = 8 * 1024;

/// Shared API state.
#[derive(Clone)]
pub struct ApiState {
    token: Arc<str>,
    port: u16,
    daemon_version: Arc<str>,
    started_at_us: i64,
    database_status: Arc<str>,
    key_protection: Arc<str>,
    store: Option<Store>,
    session_controller: Option<Arc<dyn SessionController>>,
    recovery_controller: Option<Arc<dyn RecoveryController>>,
    adapter_controller: Option<Arc<dyn AdapterController>>,
    exports_root: Option<Arc<PathBuf>>,
    blob_store: Option<BlobStore>,
}

impl ApiState {
    /// Constructs API state for one daemon instance.
    #[must_use]
    pub fn new(
        token: impl Into<Arc<str>>,
        port: u16,
        daemon_version: impl Into<Arc<str>>,
        started_at_us: i64,
    ) -> Self {
        Self {
            token: token.into(),
            port,
            daemon_version: daemon_version.into(),
            started_at_us,
            database_status: Arc::from("not_initialized"),
            key_protection: Arc::from("unknown"),
            store: None,
            session_controller: None,
            recovery_controller: None,
            adapter_controller: None,
            exports_root: None,
            blob_store: None,
        }
    }

    /// Attaches the real event store and marks the database ready.
    #[must_use]
    pub fn with_store(mut self, store: Store) -> Self {
        self.store = Some(store);
        self.database_status = Arc::from("ready");
        self
    }

    /// Sets the human-readable key protection label.
    #[must_use]
    pub fn with_key_protection(mut self, label: impl Into<Arc<str>>) -> Self {
        self.key_protection = label.into();
        self
    }

    /// Attaches the daemon-owned wrapper lifecycle coordinator.
    #[must_use]
    pub fn with_session_controller(mut self, controller: Arc<dyn SessionController>) -> Self {
        self.session_controller = Some(controller);
        self
    }

    /// Attaches the daemon-owned recovery coordinator.
    #[must_use]
    pub fn with_recovery_controller(mut self, controller: Arc<dyn RecoveryController>) -> Self {
        self.recovery_controller = Some(controller);
        self
    }

    /// Attaches the daemon-owned adapter registry and importer.
    #[must_use]
    pub fn with_adapter_controller(mut self, controller: Arc<dyn AdapterController>) -> Self {
        self.adapter_controller = Some(controller);
        self
    }

    /// Sets the owner-only directory where evidence exports are written.
    #[must_use]
    pub fn with_exports_root(mut self, exports_root: PathBuf) -> Self {
        self.exports_root = Some(Arc::new(exports_root));
        self
    }

    /// Attaches the encrypted content store for demo and export operations.
    #[must_use]
    pub fn with_blob_store(mut self, blob_store: BlobStore) -> Self {
        self.blob_store = Some(blob_store);
        self
    }
}

/// Request to prepare a wrapper session before the child process starts.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareWrapperRequest {
    /// Project path or a path inside it.
    pub project_path: String,
    /// Optional user label.
    pub label: Option<String>,
    /// Optional agent name supplied by the user.
    pub agent: Option<String>,
    /// Redacted command preview.
    pub command_preview: String,
    /// Explicit fast path that skips the baseline snapshot.
    pub no_recovery_snapshot: bool,
}

/// Prepared wrapper session returned to the CLI.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareWrapperResponse {
    /// Created session identifier.
    pub session_id: String,
    /// Project identifier.
    pub project_id: String,
    /// Canonical project root.
    pub project_root: String,
    /// Baseline recovery coverage.
    pub recovery_coverage: String,
}

/// Root process registration after PTY spawn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRootPidRequest {
    /// Root child process identifier.
    pub pid: u32,
}

/// Completion request after a wrapped process exits.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteWrapperRequest {
    /// Child exit code.
    pub exit_code: u32,
    /// Termination signal when applicable.
    pub signal: Option<String>,
    /// End time in UTC microseconds.
    pub ended_at_us: i64,
}

/// Controller failure mapped to a stable local API error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionControllerError {
    /// Stable machine code.
    pub code: String,
    /// Redacted actionable message.
    pub message: String,
    /// Whether the requested session is absent.
    pub not_found: bool,
}

impl SessionControllerError {
    /// Creates an internal controller error.
    #[must_use]
    pub fn internal(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            not_found: false,
        }
    }

    /// Creates a session-not-found error.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "session_not_found".to_owned(),
            message: message.into(),
            not_found: true,
        }
    }
}

/// Daemon-owned wrapper lifecycle and observer coordination.
#[async_trait]
pub trait SessionController: Send + Sync {
    /// Creates records, baseline, and live observers.
    async fn prepare_wrapper(
        &self,
        request: PrepareWrapperRequest,
    ) -> Result<PrepareWrapperResponse, SessionControllerError>;

    /// Registers the exact PTY root PID for ancestry correlation.
    async fn register_root_pid(
        &self,
        session_id: &str,
        request: RegisterRootPidRequest,
    ) -> Result<(), SessionControllerError>;

    /// Stores one encrypted transcript chunk.
    async fn append_transcript(
        &self,
        session_id: &str,
        bytes: &[u8],
    ) -> Result<(), SessionControllerError>;

    /// Stops observers, captures final state, and finalizes the chain.
    async fn complete_wrapper(
        &self,
        session_id: &str,
        request: CompleteWrapperRequest,
    ) -> Result<(), SessionControllerError>;
}

/// Request to build an immutable recovery plan.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRecoveryRequest {
    /// Source session identifier.
    pub session_id: String,
    /// Recovery action.
    pub action: String,
    /// Destination root or file path.
    pub destination: String,
    /// Selected project-relative paths for partial restores.
    pub paths: Vec<String>,
}

/// Serializable recovery operation projection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOperationView {
    /// Project-relative path.
    pub relative_path: String,
    /// Operation kind.
    pub kind: String,
    /// Expected output hash.
    pub expected_hash: Option<String>,
    /// Expected current hash for conflict checks.
    pub expected_current_hash: Option<String>,
    /// Content size.
    pub byte_length: Option<u64>,
    /// Executable-bit state.
    pub executable: bool,
}

/// Serializable excluded recovery item.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryExclusionView {
    /// Project-relative path.
    pub relative_path: String,
    /// Exclusion reason.
    pub reason: String,
}

/// Immutable recovery plan returned to the CLI or desktop.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryPlanView {
    /// Plan identifier.
    pub plan_id: String,
    /// Source session identifier.
    pub session_id: String,
    /// Action.
    pub action: String,
    /// Destination root.
    pub destination: String,
    /// Source coverage.
    pub coverage: String,
    /// Plan digest.
    pub plan_digest: String,
    /// Operations.
    pub operations: Vec<RecoveryOperationView>,
    /// Exclusions.
    pub exclusions: Vec<RecoveryExclusionView>,
}

/// Request to execute a previously prepared plan.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteRecoveryRequest {
    /// Exact prepared plan digest.
    pub plan_digest: String,
    /// Explicit confirmation.
    pub confirm: bool,
    /// Overwrite changed files for guarded in-place recovery.
    pub overwrite_conflicts: bool,
}

/// Recovery execution result.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRunView {
    /// Run identifier.
    pub run_id: String,
    /// Plan identifier.
    pub plan_id: String,
    /// Plan that restores the encrypted pre-restore backup.
    pub backup_plan_id: Option<String>,
    /// Run state.
    pub state: String,
    /// Restored files.
    pub restored_files: u64,
    /// Skipped files.
    pub skipped_files: u64,
    /// Conflict files.
    pub conflict_files: u64,
    /// Destination.
    pub destination: String,
    /// Error code.
    pub error_code: Option<String>,
}

/// Daemon-owned recovery planning and execution.
#[async_trait]
pub trait RecoveryController: Send + Sync {
    /// Builds and persists an immutable plan.
    async fn plan_recovery(
        &self,
        request: PlanRecoveryRequest,
    ) -> Result<RecoveryPlanView, SessionControllerError>;

    /// Loads a persisted plan.
    async fn get_recovery_plan(
        &self,
        plan_id: &str,
    ) -> Result<RecoveryPlanView, SessionControllerError>;

    /// Executes a plan after digest and confirmation checks.
    async fn execute_recovery(
        &self,
        plan_id: &str,
        request: ExecuteRecoveryRequest,
    ) -> Result<RecoveryRunView, SessionControllerError>;

    /// Loads a recovery run.
    async fn get_recovery_run(
        &self,
        run_id: &str,
    ) -> Result<RecoveryRunView, SessionControllerError>;
}

/// Adapter capability projection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterCapabilityView {
    /// Capability name.
    pub name: String,
    /// Availability.
    pub availability: String,
    /// Limitation detail.
    pub detail: Option<String>,
}

/// Detected adapter projection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterView {
    /// Adapter ID.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Installation identifier.
    pub installation_id: String,
    /// Agent version.
    pub agent_version: Option<String>,
    /// Source roots.
    pub source_roots: Vec<String>,
    /// Installation status.
    pub status: String,
    /// Diagnostic code.
    pub diagnostic_code: Option<String>,
    /// Capabilities.
    pub capabilities: Vec<AdapterCapabilityView>,
}

/// Adapter scan response.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterScanResponse {
    /// Adapters found.
    pub adapters: Vec<AdapterView>,
}

/// Adapter import response.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterImportResponse {
    /// Adapter ID.
    pub adapter_id: String,
    /// Sources processed.
    pub sources: u64,
    /// Events imported.
    pub events_imported: u64,
    /// Quarantined records.
    pub quarantined: u64,
    /// Warnings.
    pub warnings: Vec<String>,
}

/// Request to install one previously previewed hook plan.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallAdapterHookRequest {
    /// Exact plan returned by the planning endpoint.
    pub plan: HookPlan,
    /// Optional idempotency key for repeated client requests.
    pub idempotency_key: Option<String>,
}

/// Request to remove AgentTraceback-owned hook entries.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallAdapterHookRequest {
    /// Optional idempotency key for repeated client requests.
    pub idempotency_key: Option<String>,
}

/// One project summary for list and dashboard views.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    /// Project identifier.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Canonical root.
    pub canonical_root: String,
    /// Version-control kind.
    pub vcs_kind: String,
    /// Last observation time in UTC microseconds.
    pub last_seen_at_us: i64,
    /// Session count.
    pub session_count: u64,
    /// Linked event count.
    pub event_count: u64,
}

/// One session summary for lists, dashboards, and detail headers.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// Session identifier.
    pub id: String,
    /// Project identifier.
    pub project_id: Option<String>,
    /// Project display name.
    pub project_name: Option<String>,
    /// Redacted title or task preview.
    pub title_preview: Option<String>,
    /// Lifecycle state.
    pub state: String,
    /// Start time in UTC microseconds.
    pub started_at_us: i64,
    /// End time in UTC microseconds.
    pub ended_at_us: Option<i64>,
    /// Agent name.
    pub agent_name: Option<String>,
    /// Harness name.
    pub harness_name: Option<String>,
    /// Provider name.
    pub provider_name: Option<String>,
    /// Model name.
    pub model_name: Option<String>,
    /// Outcome.
    pub outcome: String,
    /// Outcome confidence.
    pub outcome_confidence: String,
    /// Recovery coverage.
    pub recovery_coverage: String,
    /// Capture health.
    pub capture_health: String,
    /// Highest risk severity.
    pub risk_max_severity: String,
    /// Linked event count.
    pub event_count: u64,
    /// Changed file count.
    pub file_count: u64,
    /// Finding count.
    pub finding_count: u64,
}

/// Dashboard projection assembled from durable local records.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardView {
    /// Generation time in UTC microseconds.
    pub generated_at_us: i64,
    /// Current local-store event count.
    pub total_events: u64,
    /// Projects ordered by recent activity.
    pub projects: Vec<ProjectView>,
    /// Recent sessions.
    pub sessions: Vec<SessionView>,
    /// Most recent normalized events.
    pub recent_events: Vec<agenttraceback_types::EventEnvelope>,
    /// Highest-priority findings.
    pub findings: Vec<FindingSummary>,
}

/// One cross-session finding summary.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingSummary {
    /// Finding identifier.
    pub id: String,
    /// Session identifier.
    pub session_id: Option<String>,
    /// Event identifier.
    pub event_id: String,
    /// Project name.
    pub project_name: Option<String>,
    /// Agent name.
    pub agent_name: Option<String>,
    /// Rule identifier.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity.
    pub severity: String,
    /// Status.
    pub status: String,
    /// Title.
    pub title: String,
    /// Explanation.
    pub explanation: String,
    /// Redacted match.
    pub matched_preview: Option<String>,
    /// Creation time in UTC microseconds.
    pub created_at_us: i64,
}

/// Request to create a redacted evidence export.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    /// Session identifier.
    pub session_id: String,
    /// Export format: `json` or `markdown`.
    pub format: String,
    /// Explicitly include decrypted eligible content in a local full export.
    #[serde(default)]
    pub full: bool,
}

/// Completed local export projection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportView {
    /// Export identifier derived from the destination file name.
    pub id: String,
    /// Job state.
    pub state: String,
    /// Local destination path.
    pub path: String,
    /// Export format.
    pub format: String,
    /// Whether defense-in-depth redaction was applied.
    pub redacted: bool,
}

/// Chain verification result.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationView {
    /// Verified chain scope.
    pub scope: String,
    /// Whether the chain is internally consistent.
    pub valid: bool,
    /// Missing sequence numbers.
    pub missing_sequences: Vec<u64>,
    /// Reordered sequence numbers.
    pub reordered_sequences: Vec<u64>,
    /// Entries whose content no longer matches their hash.
    pub changed_entries: Vec<u64>,
    /// Missing encrypted payload references.
    pub missing_payloads: Vec<u64>,
    /// Whether a finalized root disagrees with the chain tail.
    pub root_mismatch: bool,
}

/// Result of installing or removing clearly labeled demo data.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoDataView {
    /// Demo project identifier.
    pub project_id: String,
    /// Whether demo data is present after the operation.
    pub installed: bool,
    /// Events added or removed.
    pub events: u64,
    /// Sessions added or removed.
    pub sessions: u64,
    /// Demo mode label that must be visible in the UI.
    pub label: String,
}

/// Cross-session file history summary.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    /// File identity.
    pub id: String,
    /// Project name.
    pub project_name: String,
    /// Current path.
    pub path: String,
    /// Sensitive content class.
    pub sensitive_class: Option<String>,
    /// First observation time.
    pub first_seen_at_us: i64,
    /// Last observation time.
    pub last_seen_at_us: i64,
    /// Version count.
    pub version_count: u64,
    /// Session count.
    pub session_count: u64,
    /// Recoverable version count.
    pub recoverable_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerificationParams {
    session_id: Option<String>,
    all: Option<bool>,
}

/// Daemon-owned adapter detection and import.
#[async_trait]
pub trait AdapterController: Send + Sync {
    /// Lists detected built-in adapters.
    async fn list_adapters(&self) -> Result<AdapterScanResponse, SessionControllerError>;
    /// Rescans known locations and persists installations.
    async fn scan_adapters(&self) -> Result<AdapterScanResponse, SessionControllerError>;
    /// Imports all currently discovered sources for one adapter.
    async fn import_adapter(
        &self,
        adapter_id: &str,
    ) -> Result<AdapterImportResponse, SessionControllerError>;
    /// Returns the exact reversible configuration plan for one adapter.
    async fn plan_adapter_hook(
        &self,
        adapter_id: &str,
    ) -> Result<Option<HookPlan>, SessionControllerError>;
    /// Applies an explicitly approved hook plan.
    async fn install_adapter_hook(
        &self,
        adapter_id: &str,
        request: InstallAdapterHookRequest,
    ) -> Result<HookReceipt, SessionControllerError>;
    /// Removes the persisted AgentTraceback-owned hook receipt.
    async fn uninstall_adapter_hook(
        &self,
        adapter_id: &str,
        _request: UninstallAdapterHookRequest,
    ) -> Result<(), SessionControllerError>;
}

/// Daemon and subsystem health.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Overall daemon status.
    pub status: String,
    /// API compatibility version.
    pub api_version: u32,
    /// Daemon package version.
    pub daemon_version: String,
    /// Daemon process identifier.
    pub pid: u32,
    /// Loopback port.
    pub port: u16,
    /// Daemon start timestamp in UTC microseconds.
    pub started_at_us: i64,
    /// Database subsystem state.
    pub database: SubsystemHealth,
    /// Capture subsystem state.
    pub capture: SubsystemHealth,
    /// Owner-visible key protection status.
    pub key_protection: String,
}

/// A health entry for one subsystem.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubsystemHealth {
    /// Stable status code.
    pub status: String,
    /// Human-readable redacted details.
    pub detail: Option<String>,
}

/// Platform and global capture capability response.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesResponse {
    /// Operating-system platform name.
    pub platform: String,
    /// Whether a Deep Capture provider is active.
    pub deep_capture: bool,
    /// Honest standard-mode capability declarations.
    pub standard_capture: StandardCaptureCapabilities,
}

/// Standard-mode capability declarations.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardCaptureCapabilities {
    /// Exact top-level wrapped command.
    pub exact_wrapper_command: bool,
    /// Host-observed project file writes.
    pub host_observed_writes: bool,
    /// Host-observed project file reads.
    pub host_observed_reads: bool,
    /// Host-observed network requests.
    pub host_observed_network: bool,
    /// Best-effort process ancestry.
    pub best_effort_process_ancestry: bool,
    /// Git before/after state.
    pub git_boundaries: bool,
}

/// Builds the versioned router for a daemon instance.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/events/{id}", get(get_event))
        .route("/api/v1/search", get(search))
        .route("/api/v1/dashboard", get(dashboard))
        .route("/api/v1/projects", get(list_projects))
        .route("/api/v1/sessions", get(list_sessions))
        .route("/api/v1/sessions/{id}", get(get_session))
        .route("/api/v1/sessions/{id}/events", get(session_events))
        .route("/api/v1/events/recent", get(recent_events))
        .route("/api/v1/findings", get(list_findings))
        .route("/api/v1/files", get(list_files))
        .route("/api/v1/exports", post(create_export))
        .route("/api/v1/verify", get(verify_chain))
        .route("/api/v1/demo/install", post(install_demo))
        .route("/api/v1/demo/remove", post(remove_demo))
        .route("/api/v1/storage", get(storage))
        .route("/api/v1/wrapper/sessions/prepare", post(prepare_wrapper))
        .route(
            "/api/v1/wrapper/sessions/{id}/root",
            post(register_root_pid),
        )
        .route(
            "/api/v1/wrapper/sessions/{id}/transcript",
            post(append_transcript),
        )
        .route(
            "/api/v1/wrapper/sessions/{id}/complete",
            post(complete_wrapper),
        )
        .route("/api/v1/recovery/plans", post(plan_recovery))
        .route("/api/v1/recovery/plans/{id}", get(get_recovery_plan))
        .route(
            "/api/v1/recovery/plans/{id}/execute",
            post(execute_recovery),
        )
        .route("/api/v1/recovery/runs/{id}", get(get_recovery_run))
        .route("/api/v1/sessions/{id}/findings", get(session_findings))
        .route("/api/v1/sessions/{id}/files", get(session_files))
        .route("/api/v1/adapters", get(list_adapters))
        .route("/api/v1/adapters/scan", post(scan_adapters))
        .route("/api/v1/adapters/{id}/import", post(import_adapter))
        .route("/api/v1/adapters/{id}/hooks/plan", post(plan_adapter_hook))
        .route(
            "/api/v1/adapters/{id}/hooks/install",
            post(install_adapter_hook),
        )
        .route(
            "/api/v1/adapters/{id}/hooks/uninstall",
            post(uninstall_adapter_hook),
        )
        .route("/api/v1/live", get(live_socket))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_access,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Serves the API until the supplied future resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: ApiState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn enforce_access(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    validate_host(&request, state.port)?;
    let allowed_origin = validate_origin(&request)?;
    if request.method() == axum::http::Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Some(origin) = allowed_origin {
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_str(&origin).map_err(|_| {
                    ApiError::unauthorized(
                        "origin_rejected",
                        "Request Origin header is not valid ASCII.",
                    )
                })?,
            );
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, POST, OPTIONS"),
            );
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type"),
            );
            response
                .headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Origin"));
        }
        return Ok(response);
    }
    validate_bearer_token(&request, &state.token)?;
    if request.uri().query().map_or(0, str::len) > MAX_QUERY_BYTES {
        return Err(ApiError::payload_too_large(
            "query_too_large",
            "The query string exceeds the local API limit.",
        ));
    }
    let mut response = next.run(request).await;
    if let Some(origin) = allowed_origin {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(&origin).map_err(|_| {
                ApiError::unauthorized(
                    "origin_rejected",
                    "Request Origin header is not valid ASCII.",
                )
            })?,
        );
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    Ok(response)
}

fn validate_host(request: &Request, port: u16) -> Result<(), ApiError> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("host_rejected", "Missing local Host header."))?;
    let expected_ipv4 = format!("127.0.0.1:{port}");
    let expected_localhost = format!("localhost:{port}");
    if host == expected_ipv4 || host == expected_localhost {
        Ok(())
    } else {
        Err(ApiError::unauthorized(
            "host_rejected",
            "Request Host header is not the active loopback endpoint.",
        ))
    }
}

fn validate_origin(request: &Request) -> Result<Option<String>, ApiError> {
    let Some(origin) = request.headers().get(header::ORIGIN) else {
        return Ok(None);
    };
    let origin = origin.to_str().map_err(|_| {
        ApiError::unauthorized(
            "origin_rejected",
            "Request Origin header is not valid ASCII.",
        )
    })?;

    const ALLOWED_ORIGINS: [&str; 5] = [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "http://localhost:1420",
        "http://127.0.0.1:1420",
    ];
    if ALLOWED_ORIGINS.contains(&origin) {
        Ok(Some(origin.to_owned()))
    } else {
        Err(ApiError::unauthorized(
            "origin_rejected",
            "Request origin is not allowed to access the local API.",
        ))
    }
}

fn validate_bearer_token(request: &Request, expected: &str) -> Result<(), ApiError> {
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| {
            ApiError::unauthorized("authentication_required", "A bearer token is required.")
        })?;
    let supplied = authorization.as_bytes();
    let expected = expected.as_bytes();
    let is_match = supplied.len() == expected.len() && bool::from(supplied.ct_eq(expected));
    if is_match {
        Ok(())
    } else {
        Err(ApiError::unauthorized(
            "authentication_failed",
            "The bearer token is invalid.",
        ))
    }
}

async fn health(State(state): State<ApiState>) -> Result<Json<HealthResponse>, ApiError> {
    let database = match &state.store {
        Some(store) => match store.event_count().await {
            Ok(count) => SubsystemHealth {
                status: "ready".to_owned(),
                detail: Some(format!("{count} normalized events stored locally.")),
            },
            Err(error) => {
                tracing::warn!(%error, "database health check failed");
                SubsystemHealth {
                    status: "degraded".to_owned(),
                    detail: Some("The event database could not be queried.".to_owned()),
                }
            }
        },
        None => SubsystemHealth {
            status: state.database_status.to_string(),
            detail: Some("The daemon is running without a persistent event store.".to_owned()),
        },
    };
    let overall_status = if database.status == "ready" {
        "ok"
    } else {
        "degraded"
    };
    Ok(Json(HealthResponse {
        status: overall_status.to_owned(),
        api_version: API_VERSION,
        daemon_version: state.daemon_version.to_string(),
        pid: std::process::id(),
        port: state.port,
        started_at_us: state.started_at_us,
        database,
        capture: SubsystemHealth {
            status: "idle".to_owned(),
            detail: Some("No agent adapter or wrapper capture is active in this build.".to_owned()),
        },
        key_protection: state.key_protection.to_string(),
    }))
}

async fn capabilities() -> Json<CapabilitiesResponse> {
    Json(CapabilitiesResponse {
        platform: std::env::consts::OS.to_owned(),
        deep_capture: false,
        standard_capture: StandardCaptureCapabilities {
            exact_wrapper_command: true,
            host_observed_writes: true,
            host_observed_reads: false,
            host_observed_network: false,
            best_effort_process_ancestry: true,
            git_boundaries: true,
        },
    })
}

async fn live_socket(upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(handle_live_socket)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LimitParams {
    limit: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionEventsParams {
    limit: Option<u16>,
    offset: Option<u64>,
}

async fn dashboard(State(state): State<ApiState>) -> Result<Json<DashboardView>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .clone();
    let (projects, sessions, recent_events, findings, total_events) = tokio::try_join!(
        store.list_projects(50),
        store.list_sessions(50),
        store.load_recent_events(100),
        store.list_findings(50),
        store.event_count(),
    )
    .map_err(|error| {
        tracing::warn!(%error, "dashboard query failed");
        ApiError::database_unavailable()
    })?;
    Ok(Json(DashboardView {
        generated_at_us: current_time_us(),
        total_events,
        projects: projects.into_iter().map(project_view).collect(),
        sessions: sessions.into_iter().map(session_view).collect(),
        recent_events,
        findings: findings.into_iter().map(finding_view).collect(),
    }))
}

async fn list_projects(
    State(state): State<ApiState>,
    Query(params): Query<LimitParams>,
) -> Result<Json<Vec<ProjectView>>, ApiError> {
    let projects = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .list_projects(params.limit.unwrap_or(100))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "project query failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(projects.into_iter().map(project_view).collect()))
}

async fn list_sessions(
    State(state): State<ApiState>,
    Query(params): Query<LimitParams>,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    let sessions = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .list_sessions(params.limit.unwrap_or(100))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "session query failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(sessions.into_iter().map(session_view).collect()))
}

async fn get_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionView>, ApiError> {
    let session_id = session_id.parse().map_err(|_| {
        ApiError::unprocessable("invalid_session_id", "Session ID must be a UUIDv7 value.")
    })?;
    state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .get_session_summary(session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "session detail query failed");
            ApiError::database_unavailable()
        })?
        .map(session_view)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("session_not_found", "The session was not found."))
}

async fn session_events(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(params): Query<SessionEventsParams>,
) -> Result<Json<Vec<agenttraceback_types::EventEnvelope>>, ApiError> {
    let session_id = session_id.parse().map_err(|_| {
        ApiError::unprocessable("invalid_session_id", "Session ID must be a UUIDv7 value.")
    })?;
    state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .load_session_events_page(
            session_id,
            params.limit.unwrap_or(500),
            params.offset.unwrap_or(0),
        )
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "session timeline query failed");
            ApiError::database_unavailable()
        })
}

async fn recent_events(
    State(state): State<ApiState>,
    Query(params): Query<LimitParams>,
) -> Result<Json<Vec<agenttraceback_types::EventEnvelope>>, ApiError> {
    state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .load_recent_events(params.limit.unwrap_or(100))
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "recent event query failed");
            ApiError::database_unavailable()
        })
}

async fn list_findings(
    State(state): State<ApiState>,
    Query(params): Query<LimitParams>,
) -> Result<Json<Vec<FindingSummary>>, ApiError> {
    let findings = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .list_findings(params.limit.unwrap_or(100))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "finding query failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(findings.into_iter().map(finding_view).collect()))
}

async fn list_files(
    State(state): State<ApiState>,
    Query(params): Query<LimitParams>,
) -> Result<Json<Vec<FileSummary>>, ApiError> {
    let files = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .list_files(params.limit.unwrap_or(100))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "file history query failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(
        files
            .into_iter()
            .map(|file| FileSummary {
                id: file.id.to_string(),
                project_name: file.project_name,
                path: file.path,
                sensitive_class: file.sensitive_class,
                first_seen_at_us: file.first_seen_at_us,
                last_seen_at_us: file.last_seen_at_us,
                version_count: file.version_count,
                session_count: file.session_count,
                recoverable_count: file.recoverable_count,
            })
            .collect(),
    ))
}

async fn create_export(
    State(state): State<ApiState>,
    Json(request): Json<ExportRequest>,
) -> Result<(StatusCode, Json<ExportView>), ApiError> {
    let exports_root = state
        .exports_root
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?;
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .clone();
    let session_id = request.session_id.parse().map_err(|_| {
        ApiError::unprocessable("invalid_session_id", "Session ID must be a UUIDv7 value.")
    })?;
    let session = store
        .get_session_summary(session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "export session query failed");
            ApiError::database_unavailable()
        })?
        .ok_or_else(|| ApiError::not_found("session_not_found", "The session was not found."))?;
    let (events, versions, findings) = tokio::try_join!(
        store.load_session_events(session_id, 500),
        store.load_session_file_versions(session_id),
        store.load_risk_findings(session_id),
    )
    .map_err(|error| {
        tracing::warn!(%error, "export data query failed");
        ApiError::database_unavailable()
    })?;
    let mut full_content = Vec::new();
    if request.full {
        let blobs = state.blob_store.as_ref().ok_or_else(|| {
            ApiError::internal(
                "blob_store_unavailable",
                "Full export requires the encrypted local blob store.",
            )
        })?;
        for version in &versions {
            let Some(blob_id) = &version.version.blob_id else {
                continue;
            };
            let bytes = blobs.get(&decode_blob_digest(blob_id)?).map_err(|error| {
                tracing::warn!(%error, "full export blob decrypt failed");
                ApiError::internal(
                    "export_decrypt_failed",
                    "An eligible content blob could not be decrypted.",
                )
            })?;
            full_content.push(ExportContent {
                path: version.version.display_path.clone(),
                role: version.snapshot_kind.clone(),
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
        for event in &events {
            let Some(blob_id) = &event.source.raw_blob_id else {
                continue;
            };
            let bytes = blobs.get(&decode_blob_digest(blob_id)?).map_err(|error| {
                tracing::warn!(%error, "full export raw payload decrypt failed");
                ApiError::internal(
                    "export_decrypt_failed",
                    "An eligible raw payload could not be decrypted.",
                )
            })?;
            full_content.push(ExportContent {
                path: event.id.to_string(),
                role: "raw_payload".to_owned(),
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
    }
    let files = export_file_changes(versions);
    let bundle = EvidenceBundle {
        format_version: 1,
        redacted: !request.full,
        evidence_notice: if request.full {
            "FULL LOCAL EXPORT: explicitly decrypted eligible content is included. REPORTED, OBSERVED, and VERIFIED semantics are unchanged.".to_owned()
        } else {
            "REPORTED, OBSERVED, and VERIFIED preserve their stored semantics in this redacted export.".to_owned()
        },
        session: ExportSession {
            id: session.id.to_string(),
            title: session.title_preview,
            agent: session.agent_name,
            model: session.model_name,
            project: session.project_name,
            started_at_us: session.started_at_us,
            ended_at_us: session.ended_at_us,
            outcome: session.outcome,
            capture_health: session.capture_health,
            recovery_coverage: session.recovery_coverage,
            risk_max_severity: session.risk_max_severity,
        },
        events,
        files,
        findings: findings
            .into_iter()
            .map(|finding| ExportFinding {
                rule_id: finding.rule_id,
                rule_version: finding.rule_version,
                severity: finding.severity,
                status: finding.status,
                title: finding.title,
                explanation: finding.explanation,
                matched_preview: finding.matched_preview,
                created_at_us: finding.created_at_us,
            })
            .collect(),
        full_content,
    };
    let (format, extension, rendered) = match request.format.as_str() {
        "json" => ("json", "json", render_json(&bundle)),
        "markdown" | "md" => ("markdown", "md", render_markdown(&bundle)),
        _ => {
            return Err(ApiError::unprocessable(
                "invalid_export_format",
                "Export format must be `json` or `markdown`.",
            ));
        }
    };
    let rendered = rendered.map_err(|error| {
        tracing::warn!(%error, "export rendering failed");
        ApiError::internal(
            "export_render_failed",
            "The evidence bundle could not be rendered.",
        )
    })?;
    std::fs::create_dir_all(exports_root.as_ref()).map_err(|error| {
        tracing::warn!(%error, "export directory creation failed");
        ApiError::internal(
            "export_write_failed",
            "The export directory is unavailable.",
        )
    })?;
    let file_name = format!(
        "agenttraceback-{}-{}.{}",
        &session.id.to_string()[..8],
        current_time_us(),
        extension
    );
    let destination = exports_root.join(&file_name);
    let temporary = exports_root.join(format!("{file_name}.tmp"));
    std::fs::write(&temporary, rendered.as_bytes()).map_err(|error| {
        tracing::warn!(%error, "export temporary write failed");
        ApiError::internal("export_write_failed", "The export could not be written.")
    })?;
    std::fs::rename(&temporary, &destination).map_err(|error| {
        tracing::warn!(%error, "export atomic replace failed");
        ApiError::internal("export_write_failed", "The export could not be finalized.")
    })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ExportView {
            id: file_name,
            state: "succeeded".to_owned(),
            path: destination.to_string_lossy().into_owned(),
            format: format.to_owned(),
            redacted: !request.full,
        }),
    ))
}

fn decode_blob_digest(value: &str) -> Result<[u8; 32], ApiError> {
    let bytes = hex::decode(value).map_err(|_| {
        ApiError::internal(
            "invalid_blob_digest",
            "An encrypted content reference is malformed.",
        )
    })?;
    bytes.try_into().map_err(|_| {
        ApiError::internal(
            "invalid_blob_digest",
            "An encrypted content reference has an invalid length.",
        )
    })
}

async fn verify_chain(
    State(state): State<ApiState>,
    Query(params): Query<VerificationParams>,
) -> Result<Json<VerificationView>, ApiError> {
    let session_id = if let Some(session_id) = params.session_id {
        Some(session_id.parse().map_err(|_| {
            ApiError::unprocessable("invalid_session_id", "Session ID must be a UUIDv7 value.")
        })?)
    } else if params.all.unwrap_or(false) {
        None
    } else {
        return Err(ApiError::unprocessable(
            "verification_scope_required",
            "Pass `sessionId` or `all=true`.",
        ));
    };
    let report = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .verify_chain(session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "chain verification failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(VerificationView {
        scope: session_id.map_or_else(|| "global".to_owned(), |id| id.to_string()),
        valid: report.is_valid(),
        missing_sequences: report.missing_sequences,
        reordered_sequences: report.reordered_sequences,
        changed_entries: report.changed_entries,
        missing_payloads: report.missing_payloads,
        root_mismatch: report.root_mismatch,
    }))
}

async fn install_demo(State(state): State<ApiState>) -> Result<Json<DemoDataView>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .clone();
    let project_id = demo_project_id();
    let session_id = demo_session_id();
    if let Some(existing) = store
        .get_session_summary(session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo lookup failed");
            ApiError::database_unavailable()
        })?
    {
        return Ok(Json(DemoDataView {
            project_id: project_id.to_string(),
            installed: true,
            events: existing.event_count,
            sessions: 1,
            label: "DEMO DATA".to_owned(),
        }));
    }
    let now = current_time_us();
    store
        .upsert_project(agenttraceback_store::ProjectRecord {
            id: project_id,
            display_name: "AgentTraceback Demo".to_owned(),
            canonical_root: "demo://agenttraceback".to_owned(),
            comparison_root: "demo://agenttraceback".to_owned(),
            vcs_kind: "none".to_owned(),
            roots: vec![agenttraceback_store::ProjectRootRecord {
                id: EntityId::new(),
                canonical_path: "demo://agenttraceback".to_owned(),
                comparison_path: "demo://agenttraceback".to_owned(),
                root_kind: "demo".to_owned(),
                enabled: true,
            }],
            created_at_us: now,
            last_seen_at_us: now,
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo project creation failed");
            ApiError::database_unavailable()
        })?;
    store
        .create_session(agenttraceback_store::SessionRecord {
            id: session_id,
            project_id: Some(project_id),
            title_preview: Some("Demo: verified file write and recovery".to_owned()),
            state: "active".to_owned(),
            started_at_us: now,
            ended_at_us: None,
            agent_name: Some("demo-agent".to_owned()),
            harness_name: Some("AgentTraceback demo dataset".to_owned()),
            provider_name: Some("local".to_owned()),
            model_name: Some("demo-model".to_owned()),
            source_session_id: Some("agenttraceback-demo-v1".to_owned()),
            outcome: "unknown".to_owned(),
            outcome_confidence: "unknown".to_owned(),
            recovery_coverage: "metadata_only".to_owned(),
            capture_health: "demo".to_owned(),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo session creation failed");
            ApiError::database_unavailable()
        })?;

    let reported = demo_event(
        project_id,
        session_id,
        DemoEventSpec {
            occurred_at_us: now,
            source_event_id: "demo:reported:write",
            source_kind: agenttraceback_types::SourceKind::AgentLog,
            action: agenttraceback_types::EventAction::FileWrite,
            target: "src/auth.ts",
            preview: "Reported update to src/auth.ts",
        },
    );
    let observed = demo_event(
        project_id,
        session_id,
        DemoEventSpec {
            occurred_at_us: now.saturating_add(120_000),
            source_event_id: "demo:observed:write",
            source_kind: agenttraceback_types::SourceKind::FilesystemObserver,
            action: agenttraceback_types::EventAction::FileWrite,
            target: "src/auth.ts",
            preview: "Host observed src/auth.ts change",
        },
    );
    let prompt = demo_event(
        project_id,
        session_id,
        DemoEventSpec {
            occurred_at_us: now.saturating_sub(5_000_000),
            source_event_id: "demo:prompt",
            source_kind: agenttraceback_types::SourceKind::AgentLog,
            action: agenttraceback_types::EventAction::Prompt,
            target: "fix authentication",
            preview: "Fix the failing authentication test",
        },
    );
    let command = demo_event(
        project_id,
        session_id,
        DemoEventSpec {
            occurred_at_us: now.saturating_add(250_000),
            source_event_id: "demo:command",
            source_kind: agenttraceback_types::SourceKind::Wrapper,
            action: agenttraceback_types::EventAction::CommandExecute,
            target: "cargo test",
            preview: "cargo test",
        },
    );
    let test = demo_event(
        project_id,
        session_id,
        DemoEventSpec {
            occurred_at_us: now.saturating_add(350_000),
            source_event_id: "demo:test",
            source_kind: agenttraceback_types::SourceKind::Wrapper,
            action: agenttraceback_types::EventAction::TestResult,
            target: "auth test suite",
            preview: "test suite passed",
        },
    );
    let mut test = test;
    test.result.status = agenttraceback_types::ResultStatus::Success;
    let ended_at_us = test.occurred_at_us.saturating_add(500_000);
    let mut events = vec![
        prompt,
        reported.clone(),
        observed.clone(),
        command.clone(),
        test,
    ];
    let persisted = store
        .append_events(std::mem::take(&mut events))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo event append failed");
            ApiError::database_unavailable()
        })?;
    let blobs = state.blob_store.as_ref().ok_or_else(|| {
        ApiError::internal(
            "blob_store_unavailable",
            "Encrypted demo file content is unavailable.",
        )
    })?;
    let before_content = b"export const tokenMode = 'legacy';\n";
    let after_content = b"export const tokenMode = 'refreshed';\n";
    let before_blob = blobs.put(before_content).map_err(|error| {
        tracing::warn!(%error, "demo before blob failed");
        ApiError::internal("demo_blob_failed", "Demo content could not be encrypted.")
    })?;
    let after_blob = blobs.put(after_content).map_err(|error| {
        tracing::warn!(%error, "demo after blob failed");
        ApiError::internal("demo_blob_failed", "Demo content could not be encrypted.")
    })?;
    for blob in [&before_blob, &after_blob] {
        store
            .record_blob(agenttraceback_store::BlobMetadata {
                id: blob.descriptor.hex_digest.clone(),
                digest: blob.descriptor.hex_digest.clone(),
                format_version: blob.descriptor.format_version,
                plaintext_bytes: blob.descriptor.plaintext_bytes,
                encrypted_bytes: blob.descriptor.encrypted_bytes,
                media_category: "file_content".to_owned(),
                retention_class: "recovery_snapshot".to_owned(),
                created_at_us: now,
                expires_at_us: None,
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "demo blob metadata failed");
                ApiError::database_unavailable()
            })?;
    }
    let file_id = EntityId::from(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        b"agenttraceback:demo:file:auth:v1",
    ));
    store
        .upsert_file(agenttraceback_store::FileRecord {
            id: file_id,
            project_id,
            current_display_path: "src/auth.ts".to_owned(),
            current_comparison_path: "src/auth.ts".to_owned(),
            stable_file_identity: Some("demo:src/auth.ts".to_owned()),
            first_seen_at_us: now.saturating_sub(5_000_000),
            last_seen_at_us: now.saturating_add(350_000),
            sensitive_class: Some("source_code".to_owned()),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo file failed");
            ApiError::database_unavailable()
        })?;
    let before_version_id = EntityId::from(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        b"agenttraceback:demo:file-version:before:v1",
    ));
    let after_version_id = EntityId::from(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        b"agenttraceback:demo:file-version:after:v1",
    ));
    for (version_id, blob, content, created_at_us, event_id) in [
        (
            before_version_id,
            &before_blob,
            before_content.as_slice(),
            now.saturating_sub(4_000_000),
            reported.id,
        ),
        (
            after_version_id,
            &after_blob,
            after_content.as_slice(),
            now.saturating_add(120_000),
            observed.id,
        ),
    ] {
        store
            .insert_file_version(agenttraceback_store::FileVersionRecord {
                id: version_id,
                file_id,
                session_id: Some(session_id),
                event_id: Some(event_id),
                display_path_at_time: "src/auth.ts".to_owned(),
                content_hash: Some(blob.descriptor.hex_digest.clone()),
                blob_id: Some(blob.descriptor.hex_digest.clone()),
                byte_length: Some(content.len() as u64),
                media_kind: Some("text/plain".to_owned()),
                executable: Some(false),
                symlink_target: None,
                capture_status: "hashed".to_owned(),
                created_at_us,
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "demo file version failed");
                ApiError::database_unavailable()
            })?;
    }
    for (kind, version_id, completed_at_us) in [
        (
            "pre_session",
            before_version_id,
            now.saturating_sub(3_900_000),
        ),
        (
            "post_session",
            after_version_id,
            now.saturating_add(350_000),
        ),
    ] {
        store
            .save_snapshot(agenttraceback_store::SnapshotRecord {
                id: EntityId::from(uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("agenttraceback:demo:snapshot:{kind}:v1").as_bytes(),
                )),
                project_id,
                session_id: Some(session_id),
                snapshot_kind: kind.to_owned(),
                coverage: "exact".to_owned(),
                started_at_us: completed_at_us.saturating_sub(10_000),
                completed_at_us: Some(completed_at_us),
                manifest_hash: Some(format!("blake3:demo-{kind}")),
                status: "complete".to_owned(),
                entries: vec![agenttraceback_store::SnapshotEntryRecord {
                    file_id: Some(file_id),
                    display_path: "src/auth.ts".to_owned(),
                    comparison_path: "src/auth.ts".to_owned(),
                    file_version_id: Some(version_id),
                    entry_kind: "file".to_owned(),
                    capture_status: "hashed".to_owned(),
                    git_object_id: None,
                }],
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "demo snapshot failed");
                ApiError::database_unavailable()
            })?;
    }
    store
        .save_correlation(
            Some(agenttraceback_store::CorrelationInsert {
                id: EntityId::new(),
                session_id: Some(session_id),
                logical_event_id: reported.id,
                reported_event_id: reported.id,
                observed_event_id: observed.id,
                correlation_kind: "file_write_after_hash".to_owned(),
                confidence: "high".to_owned(),
                algorithm_version: 1,
            }),
            None,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo correlation failed");
            ApiError::database_unavailable()
        })?;
    store
        .save_risk_findings(vec![agenttraceback_store::RiskFindingRecord {
            id: EntityId::new(),
            session_id: Some(session_id),
            event_id: command.id,
            rule_id: "demo.sensitive_file".to_owned(),
            rule_version: "1".to_owned(),
            severity: "high".to_owned(),
            initial_status: "open".to_owned(),
            title: "Demo finding: sensitive configuration change".to_owned(),
            explanation: "Synthetic demo data showing how an explainable finding is attached to a command event."
                .to_owned(),
            matched_preview: Some("src/auth.ts".to_owned()),
            created_at_us: now,
        }])
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo finding failed");
            ApiError::database_unavailable()
        })?;
    store
        .complete_session(
            session_id,
            ended_at_us,
            "success".to_owned(),
            "demo".to_owned(),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo completion failed");
            ApiError::database_unavailable()
        })?;
    store
        .finalize_chain(Some(session_id))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo finalization failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(DemoDataView {
        project_id: project_id.to_string(),
        installed: true,
        events: persisted.len() as u64,
        sessions: 1,
        label: "DEMO DATA".to_owned(),
    }))
}

async fn remove_demo(State(state): State<ApiState>) -> Result<Json<DemoDataView>, ApiError> {
    let project_id = demo_project_id();
    let events = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?
        .delete_project_data(project_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "demo removal failed");
            ApiError::internal(
                "demo_remove_failed",
                "The demo dataset could not be removed safely.",
            )
        })?;
    Ok(Json(DemoDataView {
        project_id: project_id.to_string(),
        installed: false,
        events,
        sessions: u64::from(events > 0),
        label: "DEMO DATA".to_owned(),
    }))
}

fn demo_project_id() -> EntityId {
    EntityId::from(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        b"agenttraceback:demo:project:v1",
    ))
}

fn demo_session_id() -> EntityId {
    EntityId::from(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        b"agenttraceback:demo:session:v1",
    ))
}

struct DemoEventSpec<'a> {
    occurred_at_us: i64,
    source_event_id: &'a str,
    source_kind: agenttraceback_types::SourceKind,
    action: agenttraceback_types::EventAction,
    target: &'a str,
    preview: &'a str,
}

fn demo_event(
    project_id: EntityId,
    session_id: EntityId,
    spec: DemoEventSpec<'_>,
) -> agenttraceback_types::EventEnvelope {
    let mut event = agenttraceback_types::EventEnvelope::new(
        agenttraceback_types::EventSource {
            kind: spec.source_kind,
            original_source_kind: None,
            adapter_id: Some("agenttraceback-demo".to_owned()),
            source_event_id: spec.source_event_id.to_owned(),
            raw_blob_id: None,
        },
        spec.action,
        spec.occurred_at_us,
        spec.occurred_at_us,
    );
    event.project_id = Some(project_id);
    event.session_id = Some(session_id);
    event.actor.agent = Some("demo-agent".to_owned());
    event.target.kind = if matches!(
        spec.action,
        agenttraceback_types::EventAction::FileWrite
            | agenttraceback_types::EventAction::FileRead
            | agenttraceback_types::EventAction::FileCreate
            | agenttraceback_types::EventAction::FileDelete
    ) {
        agenttraceback_types::TargetKind::File
    } else if matches!(
        spec.action,
        agenttraceback_types::EventAction::CommandExecute
            | agenttraceback_types::EventAction::CommandResult
    ) {
        agenttraceback_types::TargetKind::Command
    } else {
        agenttraceback_types::TargetKind::Session
    };
    event.target.display = Some(spec.target.to_owned());
    event.target.normalized_path = (event.target.kind == agenttraceback_types::TargetKind::File)
        .then(|| spec.target.to_owned());
    if event.target.kind == agenttraceback_types::TargetKind::File {
        event.content.before_hash = Some("blake3:demo-before".to_owned());
        event.content.after_hash = Some("blake3:demo-after".to_owned());
    }
    event.content.redacted_preview = Some(spec.preview.to_owned());
    event
}

fn export_file_changes(
    versions: Vec<agenttraceback_store::SessionFileVersion>,
) -> Vec<ExportFileChange> {
    let mut changes = std::collections::BTreeMap::<String, ExportFileChange>::new();
    for item in versions {
        let entry = changes
            .entry(item.version.comparison_path.clone())
            .or_insert_with(|| ExportFileChange {
                path: item.version.comparison_path.clone(),
                before_hash: None,
                after_hash: None,
                capture_status: item.version.capture_status.clone(),
                before_bytes: None,
                after_bytes: None,
            });
        if item.snapshot_kind == "pre_session" {
            entry.before_hash = item.version.content_hash;
            entry.before_bytes = item.version.byte_length;
            entry.capture_status = item.version.capture_status;
        } else {
            entry.after_hash = item.version.content_hash;
            entry.after_bytes = item.version.byte_length;
        }
    }
    changes.into_values().collect()
}

fn project_view(project: agenttraceback_store::ProjectSummaryRecord) -> ProjectView {
    ProjectView {
        id: project.id.to_string(),
        display_name: project.display_name,
        canonical_root: project.canonical_root,
        vcs_kind: project.vcs_kind,
        last_seen_at_us: project.last_seen_at_us,
        session_count: project.session_count,
        event_count: project.event_count,
    }
}

fn session_view(session: agenttraceback_store::SessionSummaryRecord) -> SessionView {
    SessionView {
        id: session.id.to_string(),
        project_id: session.project_id.map(|id| id.to_string()),
        project_name: session.project_name,
        title_preview: session.title_preview,
        state: session.state,
        started_at_us: session.started_at_us,
        ended_at_us: session.ended_at_us,
        agent_name: session.agent_name,
        harness_name: session.harness_name,
        provider_name: session.provider_name,
        model_name: session.model_name,
        outcome: session.outcome,
        outcome_confidence: session.outcome_confidence,
        recovery_coverage: session.recovery_coverage,
        capture_health: session.capture_health,
        risk_max_severity: session.risk_max_severity,
        event_count: session.event_count,
        file_count: session.file_count,
        finding_count: session.finding_count,
    }
}

fn finding_view(finding: agenttraceback_store::FindingSummaryRecord) -> FindingSummary {
    FindingSummary {
        id: finding.id.to_string(),
        session_id: finding.session_id.map(|id| id.to_string()),
        event_id: finding.event_id.to_string(),
        project_name: finding.project_name,
        agent_name: finding.agent_name,
        rule_id: finding.rule_id,
        rule_version: finding.rule_version,
        severity: finding.severity,
        status: finding.status,
        title: finding.title,
        explanation: finding.explanation,
        matched_preview: finding.matched_preview,
        created_at_us: finding.created_at_us,
    }
}

fn current_time_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchParams {
    q: String,
    limit: Option<u16>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// Matching events.
    pub items: Vec<SearchItem>,
    /// Whether more results exist.
    pub has_more: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    /// Event identifier.
    pub event_id: String,
    /// Session identifier.
    pub session_id: Option<String>,
    /// Event time.
    pub occurred_at_us: i64,
    /// Normalized action.
    pub action: String,
    /// Agent name.
    pub agent: Option<String>,
    /// Model name.
    pub model: Option<String>,
    /// Project name.
    pub project_name: Option<String>,
    /// Target display.
    pub target_display: Option<String>,
    /// Redacted preview.
    pub redacted_preview: Option<String>,
    /// Evidence class.
    pub evidence: String,
    /// Result status.
    pub status: String,
    /// Risk severity.
    pub risk: String,
    /// Search rank.
    pub rank: Option<f64>,
}

async fn search(
    State(state): State<ApiState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?;
    let query = SearchQuery::parse(&params.q)
        .map_err(|error| ApiError::unprocessable("invalid_query", error.to_string()))?;
    let page = store
        .search(query, params.limit.unwrap_or(50))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "search failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(SearchResponse {
        items: page
            .items
            .into_iter()
            .map(|hit| SearchItem {
                event_id: hit.event_id.to_string(),
                session_id: hit.session_id.map(|id| id.to_string()),
                occurred_at_us: hit.occurred_at_us,
                action: hit.action,
                agent: hit.agent,
                model: hit.model,
                project_name: hit.project_name,
                target_display: hit.target_display,
                redacted_preview: hit.redacted_preview,
                evidence: hit.evidence,
                status: hit.status,
                risk: hit.risk,
                rank: hit.rank,
            })
            .collect(),
        has_more: page.has_more,
    }))
}

async fn get_event(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<agenttraceback_types::EventEnvelope>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?;
    let id = id.parse().map_err(|_| {
        ApiError::unprocessable("invalid_event_id", "Event ID must be a UUIDv7 value.")
    })?;
    store
        .get_event(id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "event retrieval failed");
            ApiError::database_unavailable()
        })?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("event_not_found", "The event does not exist."))
}

async fn storage(State(state): State<ApiState>) -> Result<Json<StorageResponse>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?;
    let event_count = store.event_count().await.map_err(|error| {
        tracing::warn!(%error, "storage summary failed");
        ApiError::database_unavailable()
    })?;
    Ok(Json(StorageResponse {
        database_status: "ready".to_owned(),
        event_count,
        soft_budget_bytes: 20 * 1024 * 1024 * 1024_u64,
        usage_bytes: None,
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageResponse {
    database_status: String,
    event_count: u64,
    soft_budget_bytes: u64,
    usage_bytes: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFinding {
    /// Finding identifier.
    pub id: String,
    /// Event identifier.
    pub event_id: String,
    /// Rule identifier.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity.
    pub severity: String,
    /// Status.
    pub status: String,
    /// Title.
    pub title: String,
    /// Explanation.
    pub explanation: String,
    /// Redacted match.
    pub matched_preview: Option<String>,
    /// Creation time.
    pub created_at_us: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChange {
    /// Project-relative path.
    pub path: String,
    /// Before-session version.
    pub before: Option<FileVersionView>,
    /// After-session version.
    pub after: Option<FileVersionView>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVersionView {
    /// File version identifier.
    pub version_id: String,
    /// BLAKE3 digest.
    pub content_hash: Option<String>,
    /// Byte length.
    pub byte_length: Option<u64>,
    /// Capture status.
    pub capture_status: String,
    /// Executable bit.
    pub executable: bool,
    /// Symlink target.
    pub symlink_target: Option<String>,
}

async fn session_findings(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<SessionFinding>>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?;
    let session_id = session_id.parse().map_err(|_| {
        ApiError::unprocessable("invalid_session_id", "Session ID must be a UUIDv7 value.")
    })?;
    let findings = store
        .load_risk_findings(session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "finding query failed");
            ApiError::database_unavailable()
        })?;
    Ok(Json(
        findings
            .into_iter()
            .map(|finding| SessionFinding {
                id: finding.id.to_string(),
                event_id: finding.event_id.to_string(),
                rule_id: finding.rule_id,
                rule_version: finding.rule_version,
                severity: finding.severity,
                status: finding.status,
                title: finding.title,
                explanation: finding.explanation,
                matched_preview: finding.matched_preview,
                created_at_us: finding.created_at_us,
            })
            .collect(),
    ))
}

async fn session_files(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<SessionFileChange>>, ApiError> {
    let store = state
        .store
        .as_ref()
        .ok_or_else(ApiError::database_unavailable)?;
    let session_id = session_id.parse().map_err(|_| {
        ApiError::unprocessable("invalid_session_id", "Session ID must be a UUIDv7 value.")
    })?;
    let versions = store
        .load_session_file_versions(session_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "session file query failed");
            ApiError::database_unavailable()
        })?;
    let mut changes = std::collections::BTreeMap::<
        String,
        (Option<FileVersionView>, Option<FileVersionView>),
    >::new();
    for item in versions {
        let view = FileVersionView {
            version_id: item.version.file_version_id.to_string(),
            content_hash: item.version.content_hash,
            byte_length: item.version.byte_length,
            capture_status: item.version.capture_status,
            executable: item.version.executable,
            symlink_target: item.version.symlink_target,
        };
        let entry = changes
            .entry(item.version.comparison_path)
            .or_insert((None, None));
        if item.snapshot_kind == "pre_session" {
            entry.0 = Some(view);
        } else {
            entry.1 = Some(view);
        }
    }
    Ok(Json(
        changes
            .into_iter()
            .map(|(path, (before, after))| SessionFileChange {
                path,
                before,
                after,
            })
            .collect(),
    ))
}

async fn prepare_wrapper(
    State(state): State<ApiState>,
    Json(request): Json<PrepareWrapperRequest>,
) -> Result<Json<PrepareWrapperResponse>, ApiError> {
    controller(&state)?
        .prepare_wrapper(request)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn register_root_pid(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<RegisterRootPidRequest>,
) -> Result<StatusCode, ApiError> {
    controller(&state)?
        .register_root_pid(&session_id, request)
        .await
        .map_err(ApiError::controller)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn append_transcript(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    controller(&state)?
        .append_transcript(&session_id, &body)
        .await
        .map_err(ApiError::controller)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn complete_wrapper(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Json(request): Json<CompleteWrapperRequest>,
) -> Result<StatusCode, ApiError> {
    controller(&state)?
        .complete_wrapper(&session_id, request)
        .await
        .map_err(ApiError::controller)?;
    Ok(StatusCode::NO_CONTENT)
}

fn controller(state: &ApiState) -> Result<&dyn SessionController, ApiError> {
    state
        .session_controller
        .as_deref()
        .ok_or_else(ApiError::database_unavailable)
}

async fn plan_recovery(
    State(state): State<ApiState>,
    Json(request): Json<PlanRecoveryRequest>,
) -> Result<Json<RecoveryPlanView>, ApiError> {
    recovery_controller(&state)?
        .plan_recovery(request)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn get_recovery_plan(
    State(state): State<ApiState>,
    Path(plan_id): Path<String>,
) -> Result<Json<RecoveryPlanView>, ApiError> {
    recovery_controller(&state)?
        .get_recovery_plan(&plan_id)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn execute_recovery(
    State(state): State<ApiState>,
    Path(plan_id): Path<String>,
    Json(request): Json<ExecuteRecoveryRequest>,
) -> Result<Json<RecoveryRunView>, ApiError> {
    recovery_controller(&state)?
        .execute_recovery(&plan_id, request)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn get_recovery_run(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<RecoveryRunView>, ApiError> {
    recovery_controller(&state)?
        .get_recovery_run(&run_id)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

fn recovery_controller(state: &ApiState) -> Result<&dyn RecoveryController, ApiError> {
    state
        .recovery_controller
        .as_deref()
        .ok_or_else(ApiError::database_unavailable)
}

async fn list_adapters(
    State(state): State<ApiState>,
) -> Result<Json<AdapterScanResponse>, ApiError> {
    adapter_controller(&state)?
        .list_adapters()
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn scan_adapters(
    State(state): State<ApiState>,
) -> Result<Json<AdapterScanResponse>, ApiError> {
    adapter_controller(&state)?
        .scan_adapters()
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn import_adapter(
    State(state): State<ApiState>,
    Path(adapter_id): Path<String>,
) -> Result<Json<AdapterImportResponse>, ApiError> {
    adapter_controller(&state)?
        .import_adapter(&adapter_id)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn plan_adapter_hook(
    State(state): State<ApiState>,
    Path(adapter_id): Path<String>,
) -> Result<Json<Option<HookPlan>>, ApiError> {
    adapter_controller(&state)?
        .plan_adapter_hook(&adapter_id)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn install_adapter_hook(
    State(state): State<ApiState>,
    Path(adapter_id): Path<String>,
    Json(request): Json<InstallAdapterHookRequest>,
) -> Result<Json<HookReceipt>, ApiError> {
    adapter_controller(&state)?
        .install_adapter_hook(&adapter_id, request)
        .await
        .map(Json)
        .map_err(ApiError::controller)
}

async fn uninstall_adapter_hook(
    State(state): State<ApiState>,
    Path(adapter_id): Path<String>,
    Json(request): Json<UninstallAdapterHookRequest>,
) -> Result<StatusCode, ApiError> {
    adapter_controller(&state)?
        .uninstall_adapter_hook(&adapter_id, request)
        .await
        .map_err(ApiError::controller)?;
    Ok(StatusCode::NO_CONTENT)
}

fn adapter_controller(state: &ApiState) -> Result<&dyn AdapterController, ApiError> {
    state
        .adapter_controller
        .as_deref()
        .ok_or_else(ApiError::database_unavailable)
}

async fn handle_live_socket(mut socket: WebSocket) {
    let message = serde_json::json!({
        "version": 1,
        "type": "capture.health_changed",
        "sequence": 1,
        "sentAt": chrono::Utc::now().to_rfc3339(),
        "data": {
            "status": "idle",
            "detail": "AgentTraceback API connection established."
        }
    });
    let _ = socket.send(Message::Text(message.to_string().into())).await;
    while let Some(Ok(message)) = socket.recv().await {
        if matches!(message, Message::Close(_)) {
            break;
        }
    }
}

async fn not_found() -> ApiError {
    ApiError::not_found(
        "route_not_found",
        "The requested local API route does not exist.",
    )
}

/// Structured local API error.
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn unauthorized(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code,
            message: message.to_owned(),
        }
    }

    fn not_found(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message: message.to_owned(),
        }
    }

    fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message: message.into(),
        }
    }

    fn internal(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
            message: message.to_owned(),
        }
    }

    fn payload_too_large(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code,
            message: message.to_owned(),
        }
    }

    fn database_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "database_unavailable",
            message: "The local event database is temporarily unavailable.".to_owned(),
        }
    }

    fn controller(error: SessionControllerError) -> Self {
        Self {
            status: if error.not_found {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            },
            code: if error.not_found {
                "session_not_found"
            } else {
                "session_controller_failed"
            },
            message: error.message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            error: ApiErrorDetail {
                code: self.code.to_owned(),
                message: self.message,
                details: serde_json::json!({}),
                request_id: uuid::Uuid::now_v7().to_string(),
            },
        };
        (self.status, Json(body)).into_response()
    }
}

/// Request timeout used by local CLI clients.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use base64::Engine as _;
    use tower::ServiceExt;

    use agenttraceback_blobs::BlobStore;
    use agenttraceback_crypto::{BlobCipher, MasterKey};
    use agenttraceback_store::Store;
    use agenttraceback_types::{EventAction, EventEnvelope, EventSource, SourceKind};

    use super::{ApiState, router};

    fn test_state() -> ApiState {
        ApiState::new("a".repeat(64), 43123, "test", 0)
    }

    #[tokio::test]
    async fn health_requires_bearer_token() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header("host", "127.0.0.1:43123")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_accepts_authenticated_loopback_request() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn hostile_browser_origin_is_rejected() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header("host", "127.0.0.1:43123")
                    .header("origin", "https://attacker.example")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn search_returns_a_stored_redacted_event() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let mut event = EventEnvelope::new(
            EventSource {
                kind: SourceKind::AgentLog,
                original_source_kind: None,
                adapter_id: Some("codex".to_owned()),
                source_event_id: "api-search-1".to_owned(),
                raw_blob_id: None,
            },
            EventAction::Prompt,
            1_799_000_000_000_000,
            1_799_000_000_000_001,
        );
        event.content.redacted_preview = Some("needle in the timeline".to_owned());
        store.append_events(vec![event]).await.expect("append");

        let state = ApiState::new("a".repeat(64), 43123, "test", 0).with_store(store.clone());
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/search?q=needle")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            body["items"][0]["redactedPreview"],
            "needle in the timeline"
        );
        store.close().await.expect("close store");
    }

    #[tokio::test]
    async fn demo_data_is_verified_labeled_exportable_and_removable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = Store::open(
            directory.path().join("agenttraceback.db"),
            directory.path().join("backups"),
        )
        .await
        .expect("store");
        let exports_root = directory.path().join("exports");
        let cipher = BlobCipher::from_master_key(&MasterKey::generate()).expect("cipher");
        let blobs = BlobStore::open(directory.path().join("blobs"), cipher).expect("blobs");
        let state = ApiState::new("a".repeat(64), 43123, "test", 0)
            .with_store(store.clone())
            .with_exports_root(exports_root.clone())
            .with_blob_store(blobs);
        let application = router(state);

        let install = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/demo/install")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        let install_status = install.status();
        let install_body = axum::body::to_bytes(install.into_body(), 64 * 1024)
            .await
            .expect("install body");
        assert_eq!(
            install_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&install_body)
        );

        let sessions = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/sessions")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let sessions_body = axum::body::to_bytes(sessions.into_body(), 64 * 1024)
            .await
            .expect("body");
        let sessions: serde_json::Value =
            serde_json::from_slice(&sessions_body).expect("sessions json");
        let session_id = sessions[0]["id"].as_str().expect("session id").to_owned();

        let timeline = application
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/sessions/{session_id}/events?limit=20"))
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let timeline_body = axum::body::to_bytes(timeline.into_body(), 64 * 1024)
            .await
            .expect("body");
        let timeline: serde_json::Value =
            serde_json::from_slice(&timeline_body).expect("timeline json");
        assert!(timeline.as_array().expect("events").iter().any(|event| {
            event["evidence"]["class"] == "verified"
                && event["target"]["normalizedPath"] == "src/auth.ts"
        }));

        let export = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/exports")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        "{{\"sessionId\":\"{session_id}\",\"format\":\"json\"}}"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(export.status(), StatusCode::ACCEPTED);

        let full_export = application
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/exports")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        "{{\"sessionId\":\"{session_id}\",\"format\":\"json\",\"full\":true}}"
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");
        let full_status = full_export.status();
        let full_body = axum::body::to_bytes(full_export.into_body(), 64 * 1024)
            .await
            .expect("full export body");
        assert_eq!(
            full_status,
            StatusCode::ACCEPTED,
            "{}",
            String::from_utf8_lossy(&full_body)
        );
        let full: serde_json::Value = serde_json::from_slice(&full_body).expect("full export json");
        assert_eq!(full["redacted"], false);
        let full_path = full["path"].as_str().expect("full export path");
        let full_content = std::fs::read_to_string(full_path).expect("full export content");
        let full_json: serde_json::Value =
            serde_json::from_str(&full_content).expect("full export json content");
        let decoded = full_json["fullContent"]
            .as_array()
            .expect("full content array")
            .iter()
            .filter_map(|item| item["bytesBase64"].as_str())
            .filter_map(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .ok()
            })
            .any(|bytes| String::from_utf8_lossy(&bytes).contains("legacy"));
        assert!(decoded);

        let remove = application
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/demo/remove")
                    .header("host", "127.0.0.1:43123")
                    .header("authorization", format!("Bearer {}", "a".repeat(64)))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        let remove_status = remove.status();
        let remove_body = axum::body::to_bytes(remove.into_body(), 64 * 1024)
            .await
            .expect("remove body");
        assert_eq!(
            remove_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&remove_body)
        );
        assert_eq!(store.event_count().await.expect("event count"), 0);
        assert!(exports_root.read_dir().expect("exports").next().is_some());
        store.close().await.expect("close store");
    }
}
