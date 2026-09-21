//! Stable domain and boundary types shared by AgentTraceback components.

mod event;

pub use event::{
    AttributionConfidence, EventAction, EventActor, EventContent, EventEnvelope, EventEvidence,
    EventIntegrity, EventResult, EventRisk, EventSource, EventTarget, EventValidationError,
    EvidenceClass, MAX_PREVIEW_BYTES, ResultStatus, RiskSeverity, SourceKind, TargetKind,
};

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The current normalized event schema version.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// The current local API version exposed under `/api/v1`.
pub const API_VERSION: u32 = 1;

/// A UUIDv7 identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityId(Uuid);

impl EntityId {
    /// Generates a time-ordered identifier suitable for persisted entities.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }

    /// Returns the all-zero identifier used for canonical comparisons.
    #[must_use]
    pub const fn nil() -> Self {
        Self(Uuid::nil())
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for EntityId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for EntityId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

/// Connection metadata written for a running per-user daemon.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeMetadata {
    /// Operating-system process identifier.
    pub pid: u32,
    /// Loopback TCP port.
    pub port: u16,
    /// Per-installation bearer token.
    pub token: String,
    /// Daemon package version.
    pub daemon_version: String,
    /// Local API version.
    pub api_version: u32,
}

/// A stable machine-readable API error.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    /// Error details.
    pub error: ApiErrorDetail,
}

/// Detailed local API error.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorDetail {
    /// Stable machine code.
    pub code: String,
    /// Redacted actionable message.
    pub message: String,
    /// Structured details.
    pub details: serde_json::Value,
    /// Request correlation identifier.
    pub request_id: String,
}
