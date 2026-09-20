//! Explainable, observe-only sensitive-path and command risk rules.

use std::path::Path;

use agenttraceback_types::{EntityId, EventAction, EventEnvelope, RiskSeverity};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// One explainable rule finding before persistence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskFindingDraft {
    /// Stable finding identifier.
    pub id: EntityId,
    /// Event that triggered the rule.
    pub event_id: EntityId,
    /// Session linked to the event.
    pub session_id: Option<EntityId>,
    /// Stable rule ID.
    pub rule_id: String,
    /// Rule version.
    pub rule_version: String,
    /// Severity.
    pub severity: RiskSeverity,
    /// Short title.
    pub title: String,
    /// Explainable rule match.
    pub explanation: String,
    /// Redacted evidence preview.
    pub matched_preview: Option<String>,
    /// Required remediation or caveat.
    pub remediation: String,
}

/// Sensitive path class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SensitiveClass {
    /// Environment configuration.
    EnvironmentFile,
    /// SSH key material or configuration.
    SshCredential,
    /// Cloud/provider credential storage.
    CloudCredential,
    /// Password-manager or browser credential storage.
    CredentialStore,
    /// Generic secret-bearing file.
    SecretFile,
}

impl SensitiveClass {
    /// Stable storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvironmentFile => "environment_file",
            Self::SshCredential => "ssh_credential",
            Self::CloudCredential => "cloud_credential",
            Self::CredentialStore => "credential_store",
            Self::SecretFile => "secret_file",
        }
    }
}

/// Default and user-extended sensitive path policy.
#[derive(Clone, Debug, Default)]
pub struct SensitivePathPolicy {
    custom_patterns: Vec<Regex>,
}

impl SensitivePathPolicy {
    /// Builds the default policy plus optional user regex patterns.
    pub fn new(custom_patterns: &[String]) -> Result<Self, regex::Error> {
        let custom_patterns = custom_patterns
            .iter()
            .map(|pattern| Regex::new(pattern))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { custom_patterns })
    }

    /// Classifies a normalized project-relative or absolute path.
    #[must_use]
    pub fn classify(&self, path: &Path) -> Option<SensitiveClass> {
        let path = path.to_string_lossy().replace('\\', "/");
        let lower = path.to_lowercase();
        let file_name = lower.rsplit('/').next().unwrap_or(&lower);
        if lower.contains("/.ssh/") || file_name == "id_rsa" || file_name == "id_ed25519" {
            return Some(SensitiveClass::SshCredential);
        }
        if lower.contains("/.aws/")
            || lower.contains("/.azure/")
            || lower.contains("/.config/gcloud/")
            || lower.ends_with("/.kube/config")
            || lower.ends_with("/.docker/config.json")
        {
            return Some(SensitiveClass::CloudCredential);
        }
        if lower.ends_with("/.npmrc")
            || lower.ends_with("/.pypirc")
            || lower.contains("/keychains/")
            || lower.contains("/password-manager/")
        {
            return Some(SensitiveClass::CredentialStore);
        }
        if file_name == ".env" || file_name.starts_with(".env.") {
            return Some(SensitiveClass::EnvironmentFile);
        }
        if file_name.contains("credential")
            || file_name.contains("secret")
            || file_name.contains("private_key")
        {
            return Some(SensitiveClass::SecretFile);
        }
        self.custom_patterns
            .iter()
            .any(|pattern| pattern.is_match(&path))
            .then_some(SensitiveClass::SecretFile)
    }
}

/// Deterministic observe-only risk classifier.
#[derive(Clone, Debug, Default)]
pub struct RiskEngine {
    sensitive_paths: SensitivePathPolicy,
}

impl RiskEngine {
    /// Creates a risk engine with custom sensitive path patterns.
    pub fn with_sensitive_patterns(patterns: &[String]) -> Result<Self, regex::Error> {
        Ok(Self {
            sensitive_paths: SensitivePathPolicy::new(patterns)?,
        })
    }

    /// Returns the sensitive class for a path, if any.
    #[must_use]
    pub fn sensitive_class(&self, path: &Path) -> Option<SensitiveClass> {
        self.sensitive_paths.classify(path)
    }

    /// Evaluates an event and returns explainable findings without blocking it.
    #[must_use]
    pub fn evaluate(&self, event: &EventEnvelope) -> Vec<RiskFindingDraft> {
        let mut findings = Vec::new();
        let preview = event.content.redacted_preview.as_deref();
        if let Some(path) = event.target.normalized_path.as_deref()
            && let Some(class) = self.sensitive_paths.classify(Path::new(path))
            && matches!(
                event.action,
                EventAction::FileRead
                    | EventAction::FileWrite
                    | EventAction::FileCreate
                    | EventAction::FileDelete
                    | EventAction::FileRename
                    | EventAction::ToolCall
            )
        {
            findings.push(self.finding(
                event,
                "sensitive_path",
                RiskSeverity::High,
                "Sensitive credential path activity",
                format!(
                    "The {} event targets a path classified as {}.",
                    event.action.as_str(),
                    class.as_str()
                ),
                preview.map(str::to_owned),
                "Review the agent instruction and rotate credentials if exposure may have occurred.",
            ));
        }
        if event.target.external
            && matches!(
                event.action,
                EventAction::FileWrite
                    | EventAction::FileCreate
                    | EventAction::FileDelete
                    | EventAction::FileRename
            )
        {
            findings.push(self.finding(
                event,
                "outside_project_write",
                RiskSeverity::High,
                "Write activity outside the project",
                "A file mutation was attributed to a target outside the registered project root."
                    .to_owned(),
                preview.map(str::to_owned),
                "Inspect the target path and verify the agent was authorized to change it.",
            ));
        }
        let Some(command) = preview else {
            return deduplicate(findings);
        };
        let tokens = command_tokens(command);
        if is_recursive_delete(&tokens) {
            findings.push(self.finding(
                event,
                "recursive_delete",
                RiskSeverity::Critical,
                "Recursive destructive deletion",
                "The command includes recursive deletion semantics.".to_owned(),
                Some(command.to_owned()),
                "Confirm the deletion scope before allowing the command to proceed.",
            ));
        }
        if contains_sequence(&tokens, &["git", "reset", "--hard"]) {
            findings.push(self.finding(
                event,
                "git_reset_hard",
                RiskSeverity::High,
                "Destructive Git reset",
                "The command discards uncommitted working-tree changes.".to_owned(),
                Some(command.to_owned()),
                "Preserve a recovery point before using destructive Git reset options.",
            ));
        }
        if contains_sequence(&tokens, &["git", "clean"])
            && tokens.iter().any(|token| token.contains('f'))
        {
            findings.push(self.finding(
                event,
                "git_clean_force",
                RiskSeverity::High,
                "Forced Git clean",
                "The command may permanently remove untracked files.".to_owned(),
                Some(command.to_owned()),
                "Review ignored and untracked files before running forced Git clean.",
            ));
        }
        if tokens
            .first()
            .is_some_and(|token| matches!(token.as_str(), "sudo" | "doas" | "runas"))
        {
            findings.push(self.finding(
                event,
                "privilege_elevation",
                RiskSeverity::Medium,
                "Privilege elevation",
                "The command requests elevated operating-system privileges.".to_owned(),
                Some(command.to_owned()),
                "Verify why the task requires elevated privileges.",
            ));
        }
        if is_download_execute_pipeline(&tokens) {
            findings.push(self.finding(
                event,
                "download_execute_pipeline",
                RiskSeverity::High,
                "Download-and-execute command",
                "The command pipes downloaded content into a shell or interpreter.".to_owned(),
                Some(command.to_owned()),
                "Download and inspect the artifact before executing it.",
            ));
        }
        if tokens
            .iter()
            .any(|token| matches!(token.as_str(), "powershell" | "pwsh"))
            && tokens.iter().any(|token| {
                token.eq_ignore_ascii_case("-encodedcommand") || token.eq_ignore_ascii_case("-enc")
            })
        {
            findings.push(self.finding(
                event,
                "encoded_powershell",
                RiskSeverity::High,
                "Encoded PowerShell command",
                "Encoded command text obscures the operation being executed.".to_owned(),
                Some(command.to_owned()),
                "Decode and review the script before execution.",
            ));
        }
        if is_broad_permission_change(&tokens) {
            findings.push(self.finding(
                event,
                "permission_broadening",
                RiskSeverity::High,
                "Recursive permission broadening",
                "The command grants broad permissions recursively.".to_owned(),
                Some(command.to_owned()),
                "Limit the permission change to the minimum required path and mode.",
            ));
        }
        if tokens
            .iter()
            .any(|token| matches!(token.as_str(), "shutdown" | "reboot" | "poweroff" | "halt"))
        {
            findings.push(self.finding(
                event,
                "system_shutdown",
                RiskSeverity::Medium,
                "System shutdown or reboot",
                "The command can interrupt the machine and all active sessions.".to_owned(),
                Some(command.to_owned()),
                "Confirm that shutdown or reboot is expected in this environment.",
            ));
        }
        deduplicate(findings)
    }

    #[allow(clippy::too_many_arguments)]
    fn finding(
        &self,
        event: &EventEnvelope,
        rule_id: &str,
        severity: RiskSeverity,
        title: &str,
        explanation: String,
        matched_preview: Option<String>,
        remediation: &str,
    ) -> RiskFindingDraft {
        RiskFindingDraft {
            id: EntityId::new(),
            event_id: event.id,
            session_id: event.session_id,
            rule_id: rule_id.to_owned(),
            rule_version: "1".to_owned(),
            severity,
            title: title.to_owned(),
            explanation,
            matched_preview,
            remediation: remediation.to_owned(),
        }
    }
}

fn command_tokens(command: &str) -> Vec<String> {
    shell_words::split(command)
        .unwrap_or_else(|_| command.split_whitespace().map(str::to_owned).collect())
}

fn contains_sequence(tokens: &[String], sequence: &[&str]) -> bool {
    tokens.windows(sequence.len()).any(|window| {
        window
            .iter()
            .zip(sequence)
            .all(|(token, expected)| token.eq_ignore_ascii_case(expected))
    })
}

fn is_recursive_delete(tokens: &[String]) -> bool {
    tokens
        .first()
        .is_some_and(|token| matches!(token.as_str(), "rm" | "rmdir" | "rd"))
        && tokens.iter().any(|token| {
            token.starts_with('-') && token.contains('r') || token.eq_ignore_ascii_case("/s")
        })
}

fn is_download_execute_pipeline(tokens: &[String]) -> bool {
    let has_download = tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "curl" | "wget" | "invoke-webrequest" | "iwr"
        )
    });
    let has_shell = tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "sh" | "bash" | "zsh" | "fish" | "powershell" | "pwsh" | "python" | "node"
        )
    });
    has_download && has_shell && tokens.iter().any(|token| token == "|")
}

fn is_broad_permission_change(tokens: &[String]) -> bool {
    tokens.first().is_some_and(|token| token == "chmod")
        && tokens
            .iter()
            .any(|token| token.starts_with('-') && token.contains('R'))
        && tokens.iter().any(|token| token == "777" || token == "0777")
}

fn deduplicate(findings: Vec<RiskFindingDraft>) -> Vec<RiskFindingDraft> {
    let mut seen = std::collections::HashSet::new();
    findings
        .into_iter()
        .filter(|finding| seen.insert(finding.rule_id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use agenttraceback_types::{
        EventAction, EventEnvelope, EventSource, RiskSeverity, SourceKind, TargetKind,
    };

    use super::{RiskEngine, SensitiveClass};

    fn event(action: EventAction, preview: &str) -> EventEnvelope {
        let mut event = EventEnvelope::new(
            EventSource {
                kind: SourceKind::AgentLog,
                original_source_kind: None,
                adapter_id: Some("fixture".to_owned()),
                source_event_id: "risk-1".to_owned(),
                raw_blob_id: None,
            },
            action,
            1_799_000_000_000_000,
            1_799_000_000_000_001,
        );
        event.content.redacted_preview = Some(preview.to_owned());
        event
    }

    #[test]
    fn classifies_credential_paths() {
        let policy = RiskEngine::default();
        assert_eq!(
            policy.sensitive_class(Path::new(".env.local")),
            Some(SensitiveClass::EnvironmentFile)
        );
        assert_eq!(
            policy.sensitive_class(Path::new("/home/u/.ssh/id_ed25519")),
            Some(SensitiveClass::SshCredential)
        );
    }

    #[test]
    fn dangerous_commands_are_explainable_findings() {
        let engine = RiskEngine::default();
        let findings = engine.evaluate(&event(
            EventAction::CommandExecute,
            "git reset --hard HEAD~1",
        ));
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule_id == "git_reset_hard")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.severity == RiskSeverity::High)
        );
    }

    #[test]
    fn recursive_delete_is_critical() {
        let findings = RiskEngine::default()
            .evaluate(&event(EventAction::CommandExecute, "rm -rf /tmp/project"));
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule_id == "recursive_delete")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.severity == RiskSeverity::Critical)
        );
    }

    #[test]
    fn sensitive_file_event_is_flagged() {
        let mut event = event(EventAction::FileRead, "read .env");
        event.target.kind = TargetKind::File;
        event.target.normalized_path = Some(".env".to_owned());
        let findings = RiskEngine::default().evaluate(&event);
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule_id == "sensitive_path")
        );
    }
}
