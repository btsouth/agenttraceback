//! Versioned deterministic correlation between reported and observed events.

use agenttraceback_types::{
    AttributionConfidence, EntityId, EventAction, EventEnvelope, EvidenceClass,
};

/// Current correlation algorithm version.
pub const CORRELATION_VERSION: u16 = 1;

/// Deterministic correlation decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorrelationDecision {
    /// Independent evidence agrees.
    Verified(CorrelationRecord),
    /// Independent evidence conflicts on a field that must agree.
    Conflict(CorrelationConflict),
    /// No deterministic correlation is justified.
    NoMatch,
}

/// New immutable correlation record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationRecord {
    /// Correlation identifier.
    pub id: EntityId,
    /// Session shared by both events.
    pub session_id: Option<EntityId>,
    /// Logical event identifier, currently the reported event ID.
    pub logical_event_id: EntityId,
    /// Reported source event.
    pub reported_event_id: EntityId,
    /// Independent observed event.
    pub observed_event_id: EntityId,
    /// Correlation family.
    pub correlation_kind: String,
    /// Attribution confidence.
    pub confidence: AttributionConfidence,
    /// Algorithm version.
    pub algorithm_version: u16,
}

/// Conflict record preserving both source events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationConflict {
    /// Conflict identifier.
    pub id: EntityId,
    /// Session when known.
    pub session_id: Option<EntityId>,
    /// Left source event.
    pub left_event_id: EntityId,
    /// Right source event.
    pub right_event_id: EntityId,
    /// Stable conflict reason.
    pub reason_code: String,
    /// Explainable detail.
    pub detail: String,
}

/// Deterministic file/command correlation engine.
#[derive(Clone, Copy, Debug)]
pub struct CorrelationEngine {
    window_us: i64,
    batch_window_us: i64,
}

impl Default for CorrelationEngine {
    fn default() -> Self {
        Self {
            window_us: 2_000_000,
            batch_window_us: 10_000_000,
        }
    }
}

impl CorrelationEngine {
    /// Evaluates two source events without upgrading same-boundary evidence.
    #[must_use]
    pub fn evaluate(&self, left: &EventEnvelope, right: &EventEnvelope) -> CorrelationDecision {
        let (reported, observed) = match (left.evidence.class, right.evidence.class) {
            (EvidenceClass::Reported, EvidenceClass::Observed) => (left, right),
            (EvidenceClass::Observed, EvidenceClass::Reported) => (right, left),
            _ => return CorrelationDecision::NoMatch,
        };
        if reported.session_id != observed.session_id || reported.project_id != observed.project_id
        {
            return CorrelationDecision::NoMatch;
        }
        if !matches!(
            observed.evidence.attribution,
            AttributionConfidence::Exact | AttributionConfidence::High
        ) {
            return CorrelationDecision::NoMatch;
        }
        let Some(reported_target) = target_path(reported) else {
            return CorrelationDecision::NoMatch;
        };
        let Some(observed_target) = target_path(observed) else {
            return CorrelationDecision::NoMatch;
        };
        if reported_target != observed_target
            || !compatible_actions(reported.action, observed.action)
        {
            return CorrelationDecision::NoMatch;
        }
        let time_delta = reported
            .occurred_at_us
            .saturating_sub(observed.occurred_at_us)
            .abs();
        if time_delta > self.batch_window_us {
            return CorrelationDecision::NoMatch;
        }
        if time_delta > self.window_us && reported.actor.subagent_id != observed.actor.subagent_id {
            return CorrelationDecision::NoMatch;
        }
        if let (Some(reported_hash), Some(observed_hash)) = (
            reported.content.after_hash.as_deref(),
            observed.content.after_hash.as_deref(),
        ) && reported_hash != observed_hash
        {
            return CorrelationDecision::Conflict(CorrelationConflict {
                id: EntityId::new(),
                session_id: reported.session_id,
                left_event_id: reported.id,
                right_event_id: observed.id,
                reason_code: "after_hash_mismatch".to_owned(),
                detail: "Reported and observed after-state hashes disagree.".to_owned(),
            });
        }
        CorrelationDecision::Verified(CorrelationRecord {
            id: EntityId::new(),
            session_id: reported.session_id,
            logical_event_id: reported.id,
            reported_event_id: reported.id,
            observed_event_id: observed.id,
            correlation_kind: action_family(reported.action).to_owned(),
            confidence: observed.evidence.attribution,
            algorithm_version: CORRELATION_VERSION,
        })
    }
}

fn target_path(event: &EventEnvelope) -> Option<&str> {
    event
        .target
        .normalized_path
        .as_deref()
        .or(event.target.display.as_deref())
}

fn compatible_actions(reported: EventAction, observed: EventAction) -> bool {
    action_family(reported) == action_family(observed)
}

const fn action_family(action: EventAction) -> &'static str {
    match action {
        EventAction::FileCreate | EventAction::FileWrite => "file_mutation",
        EventAction::FileDelete => "file_delete",
        EventAction::FileRename => "file_rename",
        EventAction::CommandExecute | EventAction::CommandResult => "command",
        _ => "none",
    }
}

#[cfg(test)]
mod tests {
    use agenttraceback_types::{
        AttributionConfidence, EventAction, EventEnvelope, EventSource, EvidenceClass, SourceKind,
        TargetKind,
    };

    use super::{CorrelationDecision, CorrelationEngine};

    fn event(kind: SourceKind, action: EventAction, occurred_at_us: i64) -> EventEnvelope {
        let mut event = EventEnvelope::new(
            EventSource {
                kind,
                original_source_kind: None,
                adapter_id: Some("fixture".to_owned()),
                source_event_id: format!("{kind:?}:{occurred_at_us}"),
                raw_blob_id: None,
            },
            action,
            occurred_at_us,
            occurred_at_us,
        );
        event.project_id = Some(agenttraceback_types::EntityId::new());
        event.session_id = Some(agenttraceback_types::EntityId::new());
        event.target.kind = TargetKind::File;
        event.target.normalized_path = Some("src/auth.ts".to_owned());
        event.content.after_hash = Some("same-hash".to_owned());
        event
    }

    #[test]
    fn matching_independent_sources_become_verified() {
        let mut reported = event(SourceKind::AgentLog, EventAction::FileWrite, 1_000);
        let mut observed = event(
            SourceKind::FilesystemObserver,
            EventAction::FileWrite,
            1_500,
        );
        observed.project_id = reported.project_id;
        observed.session_id = reported.session_id;
        observed.evidence.attribution = AttributionConfidence::High;
        assert!(matches!(
            CorrelationEngine::default().evaluate(&reported, &observed),
            CorrelationDecision::Verified(_)
        ));

        reported.source.kind = SourceKind::AgentHook;
        reported.evidence.class = EvidenceClass::Reported;
        assert!(matches!(
            CorrelationEngine::default().evaluate(&reported, &observed),
            CorrelationDecision::Verified(_)
        ));
    }

    #[test]
    fn same_after_hash_is_required_when_both_exist() {
        let reported = event(SourceKind::AgentLog, EventAction::FileWrite, 1_000);
        let mut observed = event(
            SourceKind::FilesystemObserver,
            EventAction::FileWrite,
            1_500,
        );
        observed.project_id = reported.project_id;
        observed.session_id = reported.session_id;
        observed.evidence.attribution = AttributionConfidence::High;
        observed.content.after_hash = Some("different".to_owned());
        assert!(matches!(
            CorrelationEngine::default().evaluate(&reported, &observed),
            CorrelationDecision::Conflict(_)
        ));
    }

    #[test]
    fn ambiguous_or_same_boundary_sources_do_not_verify() {
        let reported = event(SourceKind::AgentLog, EventAction::FileWrite, 1_000);
        let mut observed = event(
            SourceKind::FilesystemObserver,
            EventAction::FileWrite,
            1_500,
        );
        observed.project_id = reported.project_id;
        observed.session_id = reported.session_id;
        observed.evidence.attribution = AttributionConfidence::Unattributed;
        assert_eq!(
            CorrelationEngine::default().evaluate(&reported, &observed),
            CorrelationDecision::NoMatch
        );
        let second_reported = event(SourceKind::AgentHook, EventAction::FileWrite, 1_500);
        assert_eq!(
            CorrelationEngine::default().evaluate(&reported, &second_reported),
            CorrelationDecision::NoMatch
        );
    }
}
