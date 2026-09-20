//! Redacted JSON and Markdown evidence bundle generation.

use std::{fmt::Write as _, sync::LazyLock};

use agenttraceback_types::EventEnvelope;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\bsk-[A-Za-z0-9_-]{16,}\b",
        r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"(?i)(\bbearer\s+)([A-Za-z0-9._~+/-]{12,}=*)",
        r#"(?i)\b(password|passwd|pwd|secret|api[_-]?key|access[_-]?token)\s*[:=]\s*["']?([^\s"',;]{4,})"#,
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("valid export secret pattern"))
    .collect()
});

/// Export failures.
#[derive(Debug, Error)]
pub enum ExportError {
    /// JSON serialization failed.
    #[error("export serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// One file change included in an evidence bundle.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFileChange {
    /// Project-relative path.
    pub path: String,
    /// Before-session hash.
    pub before_hash: Option<String>,
    /// After-session hash.
    pub after_hash: Option<String>,
    /// Capture status for the before version.
    pub capture_status: String,
    /// Before byte length.
    pub before_bytes: Option<u64>,
    /// After byte length.
    pub after_bytes: Option<u64>,
}

/// One finding included in an evidence bundle.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFinding {
    /// Stable rule identifier.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity.
    pub severity: String,
    /// Current status.
    pub status: String,
    /// Explainable title.
    pub title: String,
    /// Rule explanation.
    pub explanation: String,
    /// Redacted matched value.
    pub matched_preview: Option<String>,
    /// Finding time.
    pub created_at_us: i64,
}

/// Explicitly decrypted content included only in a full export.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportContent {
    /// File path or event relationship.
    pub path: String,
    /// Content role such as `before`, `after`, or `raw_payload`.
    pub role: String,
    /// Base64-encoded decrypted bytes.
    pub bytes_base64: String,
}

/// Session metadata accepted by the export renderer.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSession {
    /// Session identifier.
    pub id: String,
    /// Session title.
    pub title: Option<String>,
    /// Agent name.
    pub agent: Option<String>,
    /// Model name.
    pub model: Option<String>,
    /// Project name.
    pub project: Option<String>,
    /// Start time.
    pub started_at_us: i64,
    /// End time.
    pub ended_at_us: Option<i64>,
    /// Outcome.
    pub outcome: String,
    /// Capture health.
    pub capture_health: String,
    /// Recovery coverage.
    pub recovery_coverage: String,
    /// Highest risk severity.
    pub risk_max_severity: String,
}

/// Complete redacted bundle content.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceBundle {
    /// Bundle format version.
    pub format_version: u16,
    /// Always true for the v0.1 default exporter.
    pub redacted: bool,
    /// Evidence semantics reminder.
    pub evidence_notice: String,
    /// Exported session.
    pub session: ExportSession,
    /// Normalized events with already-redacted previews.
    pub events: Vec<EventEnvelope>,
    /// File version summary.
    pub files: Vec<ExportFileChange>,
    /// Risk findings.
    pub findings: Vec<ExportFinding>,
    /// Decrypted content present only for an explicit full export.
    pub full_content: Vec<ExportContent>,
}

/// Builds a redacted JSON evidence bundle.
pub fn render_json(bundle: &EvidenceBundle) -> Result<String, ExportError> {
    let mut value = serde_json::to_value(bundle)?;
    if bundle.redacted {
        scrub_json(&mut value);
    }
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Builds a human-readable Markdown evidence report.
pub fn render_markdown(bundle: &EvidenceBundle) -> Result<String, ExportError> {
    let mut output = String::new();
    writeln!(output, "# AgentTraceback evidence report").expect("writing to string");
    writeln!(output).expect("writing to string");
    let notice = if bundle.redacted {
        "> Redacted export. `REPORTED` means an agent-controlled source said it happened; `OBSERVED` means the host recorder saw an effect; `VERIFIED` requires an active independent correlation."
    } else {
        "> FULL LOCAL EXPORT. This file contains explicitly decrypted content and may contain credentials or source code. Store it securely."
    };
    writeln!(output, "{notice}").expect("writing to string");
    writeln!(output).expect("writing to string");
    writeln!(output, "## Session").expect("writing to string");
    writeln!(output).expect("writing to string");
    writeln!(output, "- ID: {}", export_text(bundle, &bundle.session.id))
        .expect("writing to string");
    writeln!(
        output,
        "- Title: {}",
        export_text(
            bundle,
            bundle.session.title.as_deref().unwrap_or("Untitled")
        )
    )
    .expect("writing to string");
    writeln!(
        output,
        "- Agent: {}",
        export_text(bundle, bundle.session.agent.as_deref().unwrap_or("unknown"))
    )
    .expect("writing to string");
    writeln!(
        output,
        "- Model: {}",
        export_text(bundle, bundle.session.model.as_deref().unwrap_or("unknown"))
    )
    .expect("writing to string");
    writeln!(
        output,
        "- Project: {}",
        export_text(
            bundle,
            bundle.session.project.as_deref().unwrap_or("unattached"),
        )
    )
    .expect("writing to string");
    writeln!(
        output,
        "- Outcome: {}",
        export_text(bundle, &bundle.session.outcome)
    )
    .expect("writing to string");
    writeln!(
        output,
        "- Capture health: {}",
        bundle.session.capture_health
    )
    .expect("writing to string");
    writeln!(
        output,
        "- Recovery coverage: {}",
        bundle.session.recovery_coverage
    )
    .expect("writing to string");
    writeln!(output).expect("writing to string");

    writeln!(output, "## Timeline").expect("writing to string");
    writeln!(output).expect("writing to string");
    writeln!(
        output,
        "| Time (UTC us) | Evidence | Action | Target | Result | Preview |"
    )
    .expect("writing to string");
    writeln!(output, "| ---: | --- | --- | --- | --- | --- |").expect("writing to string");
    for event in &bundle.events {
        writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} |",
            event.occurred_at_us,
            event.evidence.class.as_str().to_uppercase(),
            event.action.as_str(),
            export_text(bundle, event.target.display.as_deref().unwrap_or("")),
            event.result.status.as_str(),
            export_text(
                bundle,
                event.content.redacted_preview.as_deref().unwrap_or(""),
            ),
        )
        .expect("writing to string");
    }
    writeln!(output).expect("writing to string");

    writeln!(output, "## File versions").expect("writing to string");
    writeln!(output).expect("writing to string");
    if bundle.files.is_empty() {
        writeln!(output, "No captured file versions were available.").expect("writing to string");
    } else {
        writeln!(output, "| Path | Before | After | Capture |").expect("writing to string");
        writeln!(output, "| --- | --- | --- | --- |").expect("writing to string");
        for file in &bundle.files {
            writeln!(
                output,
                "| {} | {} | {} | {} |",
                export_text(bundle, &file.path),
                export_text(bundle, file.before_hash.as_deref().unwrap_or("none")),
                export_text(bundle, file.after_hash.as_deref().unwrap_or("none")),
                export_text(bundle, &file.capture_status),
            )
            .expect("writing to string");
        }
    }
    writeln!(output).expect("writing to string");

    writeln!(output, "## Findings").expect("writing to string");
    writeln!(output).expect("writing to string");
    if bundle.findings.is_empty() {
        writeln!(output, "No findings were attached to this session.").expect("writing to string");
    } else {
        for finding in &bundle.findings {
            writeln!(
                output,
                "### {} ({}, {})",
                export_text(bundle, &finding.title),
                export_text(bundle, &finding.severity),
                finding.rule_id
            )
            .expect("writing to string");
            writeln!(output).expect("writing to string");
            writeln!(output, "{}", export_text(bundle, &finding.explanation))
                .expect("writing to string");
            writeln!(output).expect("writing to string");
        }
    }
    if !bundle.full_content.is_empty() {
        writeln!(output).expect("writing to string");
        writeln!(output, "## Decrypted content").expect("writing to string");
        writeln!(output).expect("writing to string");
        writeln!(
            output,
            "> This section is present only after explicit full-export selection."
        )
        .expect("writing to string");
        writeln!(output).expect("writing to string");
        for content in &bundle.full_content {
            writeln!(
                output,
                "### {} ({})",
                escape_markdown(&content.path),
                content.role
            )
            .expect("writing to string");
            writeln!(output).expect("writing to string");
            writeln!(output, "```base64").expect("writing to string");
            writeln!(output, "{}", content.bytes_base64).expect("writing to string");
            writeln!(output, "```").expect("writing to string");
            writeln!(output).expect("writing to string");
        }
    }
    Ok(output)
}

fn export_text(bundle: &EvidenceBundle, input: &str) -> String {
    if bundle.redacted {
        escape_markdown(&scrub_text(input))
    } else {
        escape_markdown(input)
    }
}

fn scrub_json(value: &mut Value) {
    match value {
        Value::String(text) => *text = scrub_text(text),
        Value::Array(items) => items.iter_mut().for_each(scrub_json),
        Value::Object(entries) => entries.values_mut().for_each(scrub_json),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn scrub_text(input: &str) -> String {
    let mut output = input.to_owned();
    for pattern in SECRET_PATTERNS.iter() {
        output = pattern
            .replace_all(&output, "[REDACTED:export]")
            .into_owned();
    }
    output
}

fn escape_markdown(input: &str) -> String {
    let mut output = String::new();
    for character in input.chars() {
        match character {
            '\r' | '\n' => output.push(' '),
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' => {
                output.push('\\');
                output.push(character);
            }
            _ => output.push(character),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_title_is_literal_markdown_text() {
        let mut bundle = bundle_with_secret();
        bundle.session.title = Some(
            "<img src=x onerror=alert(1)> [click](https://example.invalid)\n# forged".to_owned(),
        );
        let markdown = render_markdown(&bundle).expect("markdown");
        assert!(!markdown.contains("<img"));
        assert!(!markdown.contains("[click]("));
        assert!(!markdown.contains("\n# forged"));
        assert!(markdown.contains("&lt;img"));
    }
    use agenttraceback_types::{
        AttributionConfidence, EntityId, EventAction, EventContent, EventEnvelope, EventEvidence,
        EventResult, EventRisk, EventSource, EventTarget, EvidenceClass, ResultStatus, SourceKind,
        TargetKind,
    };

    fn bundle_with_secret() -> EvidenceBundle {
        let mut event = EventEnvelope::new(
            EventSource {
                kind: SourceKind::AgentLog,
                original_source_kind: None,
                adapter_id: Some("fixture".to_owned()),
                source_event_id: "source-1".to_owned(),
                raw_blob_id: None,
            },
            EventAction::CommandExecute,
            1_789_900_800_000_000,
            1_789_900_800_000_000,
        );
        event.id = EntityId::new();
        event.session_id = Some(EntityId::new());
        event.target = EventTarget {
            kind: TargetKind::Command,
            display: Some("export OPENAI_API_KEY=sk-secretsecretsecretsecret".to_owned()),
            normalized_path: None,
            external: false,
        };
        event.result = EventResult {
            status: ResultStatus::Success,
            exit_code: Some(0),
            duration_ms: None,
        };
        event.content = EventContent {
            redacted_preview: Some("token sk-secretsecretsecretsecret".to_owned()),
            payload_blob_id: None,
            before_hash: None,
            after_hash: None,
            bytes_changed: None,
        };
        event.evidence = EventEvidence {
            class: EvidenceClass::Reported,
            attribution: AttributionConfidence::Exact,
            correlation_version: None,
        };
        event.risk = EventRisk {
            severity: agenttraceback_types::RiskSeverity::None,
            finding_ids: Vec::new(),
        };
        EvidenceBundle {
            format_version: 1,
            redacted: true,
            evidence_notice: "Evidence is never upgraded by export.".to_owned(),
            session: ExportSession {
                id: "session-1".to_owned(),
                title: Some("fixture".to_owned()),
                agent: Some("fixture".to_owned()),
                model: None,
                project: Some("project".to_owned()),
                started_at_us: 1,
                ended_at_us: Some(2),
                outcome: "success".to_owned(),
                capture_health: "healthy".to_owned(),
                recovery_coverage: "exact".to_owned(),
                risk_max_severity: "none".to_owned(),
            },
            events: vec![event],
            files: Vec::new(),
            findings: Vec::new(),
            full_content: Vec::new(),
        }
    }

    #[test]
    fn json_export_removes_seeded_secret() {
        let rendered = render_json(&bundle_with_secret()).expect("json");
        assert!(!rendered.contains("sk-secret"));
        assert!(rendered.contains("[REDACTED:export]"));
    }

    #[test]
    fn markdown_export_removes_seeded_secret() {
        let rendered = render_markdown(&bundle_with_secret()).expect("markdown");
        assert!(!rendered.contains("sk-secret"));
        assert!(rendered.contains("REDACTED"));
    }

    #[test]
    fn full_json_export_preserves_explicit_content() {
        use base64::Engine as _;

        let mut bundle = bundle_with_secret();
        bundle.redacted = false;
        bundle.full_content.push(ExportContent {
            path: "src/auth.ts".to_owned(),
            role: "after".to_owned(),
            bytes_base64: base64::engine::general_purpose::STANDARD
                .encode(b"explicitly decrypted source"),
        });
        let rendered = render_json(&bundle).expect("json");
        assert!(!rendered.contains("[REDACTED:export]"));
        assert!(rendered.contains("ZXhwbGljaXRseSBkZWNyeXB0ZWQgc291cmNl"));
        assert!(rendered.contains("sk-secretsecretsecretsecret"));
    }
}
