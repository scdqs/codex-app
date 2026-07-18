use sha2::{Digest, Sha256};

use crate::{
    notification_store::SessionAlertState,
    protocol::{AlertEvent, AlertKind, SessionSnapshot, SessionStatus},
};

pub struct DetectionResult {
    pub next_state: SessionAlertState,
    pub events: Vec<AlertEvent>,
    pub ignored_as_stale: bool,
}

pub fn detect_alerts(
    previous: Option<&SessionAlertState>,
    snapshot: &SessionSnapshot,
    native_approval_ids: &[String],
) -> DetectionResult {
    if let Some(previous) = previous
        && snapshot.updated_at < previous.updated_at
    {
        return DetectionResult {
            next_state: previous.clone(),
            events: Vec::new(),
            ignored_as_stale: true,
        };
    }

    let first_observation = previous.is_none();
    let status_changed = previous.is_some_and(|state| state.status != snapshot.status);
    let state_cycle = previous.map_or(0, |state| state.state_cycle + u64::from(status_changed));
    let mut known_approval_ids = previous
        .map(|state| state.known_approval_ids.clone())
        .unwrap_or_default();
    let new_native_approval_ids = native_approval_ids
        .iter()
        .filter(|id| !known_approval_ids.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    for approval_id in native_approval_ids {
        if !known_approval_ids.contains(approval_id) {
            known_approval_ids.push(approval_id.clone());
        }
    }
    if known_approval_ids.len() > 256 {
        known_approval_ids.drain(0..known_approval_ids.len() - 256);
    }

    let mut next_state = SessionAlertState {
        thread_id: snapshot.thread_id.clone(),
        status: snapshot.status,
        updated_at: snapshot.updated_at,
        state_cycle,
        known_approval_ids,
        fallback_approval_cycle: previous.and_then(|state| state.fallback_approval_cycle),
    };
    if snapshot.status != SessionStatus::WaitingForApproval {
        next_state.fallback_approval_cycle = None;
    }
    if first_observation {
        return DetectionResult {
            next_state,
            events: Vec::new(),
            ignored_as_stale: false,
        };
    }

    let previous = previous.expect("previous exists after first observation");
    let absorb_late_native_ids = previous.status == SessionStatus::WaitingForApproval
        && previous.fallback_approval_cycle == Some(previous.state_cycle)
        && previous.known_approval_ids.is_empty()
        && !new_native_approval_ids.is_empty();
    let mut events = Vec::new();
    if !absorb_late_native_ids {
        for approval_id in &new_native_approval_ids {
            events.push(alert(
                snapshot,
                AlertKind::ApprovalRequired,
                stable_event_id(&["approval_required", &snapshot.thread_id, approval_id]),
            ));
        }
    }

    let completed =
        previous.status == SessionStatus::Running && snapshot.status == SessionStatus::Idle;
    let input_required = previous.status != SessionStatus::WaitingForInput
        && snapshot.status == SessionStatus::WaitingForInput;
    let error = previous.status != SessionStatus::Error && snapshot.status == SessionStatus::Error;
    let fallback_approval = previous.status != SessionStatus::WaitingForApproval
        && snapshot.status == SessionStatus::WaitingForApproval
        && new_native_approval_ids.is_empty();

    if completed {
        events.push(alert(
            snapshot,
            AlertKind::Completed,
            stable_event_id(&[
                "completed",
                &snapshot.thread_id,
                &previous.updated_at.to_string(),
                &snapshot.updated_at.to_string(),
            ]),
        ));
    }
    if input_required {
        events.push(cycle_alert(snapshot, AlertKind::InputRequired, state_cycle));
    }
    if error {
        events.push(cycle_alert(snapshot, AlertKind::Error, state_cycle));
    }
    if fallback_approval {
        next_state.fallback_approval_cycle = Some(state_cycle);
        events.push(cycle_alert(
            snapshot,
            AlertKind::ApprovalRequired,
            state_cycle,
        ));
    }

    DetectionResult {
        next_state,
        events,
        ignored_as_stale: false,
    }
}

fn cycle_alert(snapshot: &SessionSnapshot, kind: AlertKind, cycle: u64) -> AlertEvent {
    let kind_name = match kind {
        AlertKind::Completed => "completed",
        AlertKind::ApprovalRequired => "approval_required",
        AlertKind::InputRequired => "input_required",
        AlertKind::Error => "error",
    };
    alert(
        snapshot,
        kind,
        stable_event_id(&[kind_name, &snapshot.thread_id, &cycle.to_string()]),
    )
}

fn alert(snapshot: &SessionSnapshot, kind: AlertKind, event_id: String) -> AlertEvent {
    AlertEvent {
        event_id,
        kind,
        thread_id: snapshot.thread_id.clone(),
        thread_title: snapshot.title.clone(),
        occurred_at: snapshot.updated_at,
    }
}

fn stable_event_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("alert-{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_establishes_baseline_without_alert() {
        let current = snapshot(SessionStatus::Idle, 10);
        let result = detect_alerts(None, &current, &[]);
        assert!(result.events.is_empty());
        assert!(!result.ignored_as_stale);
        assert_eq!(result.next_state.status, SessionStatus::Idle);
    }

    #[test]
    fn running_to_idle_emits_completed_once() {
        let previous = state(SessionStatus::Running, 10);
        let idle = snapshot(SessionStatus::Idle, 20);
        let first = detect_alerts(Some(&previous), &idle, &[]);
        let second = detect_alerts(Some(&first.next_state), &idle, &[]);
        assert_eq!(first.events[0].kind, AlertKind::Completed);
        assert!(second.events.is_empty());
    }

    #[test]
    fn state_transitions_emit_input_error_and_native_approval_once() {
        let running = state(SessionStatus::Running, 10);
        let input = detect_alerts(
            Some(&running),
            &snapshot(SessionStatus::WaitingForInput, 20),
            &[],
        );
        assert_eq!(input.events[0].kind, AlertKind::InputRequired);
        let recovered = detect_alerts(
            Some(&input.next_state),
            &snapshot(SessionStatus::Running, 30),
            &[],
        );
        let error = detect_alerts(
            Some(&recovered.next_state),
            &snapshot(SessionStatus::Error, 40),
            &["approval-1".into()],
        );
        assert_eq!(
            error
                .events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![AlertKind::ApprovalRequired, AlertKind::Error]
        );
    }

    #[test]
    fn older_updated_at_is_ignored_without_replacing_state() {
        let previous = state(SessionStatus::Running, 20);
        let result = detect_alerts(Some(&previous), &snapshot(SessionStatus::Idle, 10), &[]);
        assert!(result.ignored_as_stale);
        assert_eq!(result.next_state, previous);
    }

    #[test]
    fn late_native_id_after_fallback_does_not_emit_a_second_approval_alert() {
        let baseline = state(SessionStatus::Running, 10);
        let fallback = detect_alerts(
            Some(&baseline),
            &snapshot(SessionStatus::WaitingForApproval, 20),
            &[],
        );
        let late_native = detect_alerts(
            Some(&fallback.next_state),
            &snapshot(SessionStatus::WaitingForApproval, 21),
            &["approval-1".into()],
        );
        assert_eq!(fallback.events[0].kind, AlertKind::ApprovalRequired);
        assert!(late_native.events.is_empty());
    }

    fn snapshot(status: SessionStatus, updated_at: u64) -> SessionSnapshot {
        SessionSnapshot {
            thread_id: "thread-1".into(),
            title: "Release".into(),
            cwd: None,
            model_provider: None,
            preview: None,
            updated_at,
            status,
            pending_approval_ids: Vec::new(),
        }
    }

    fn state(status: SessionStatus, updated_at: u64) -> SessionAlertState {
        SessionAlertState {
            thread_id: "thread-1".into(),
            status,
            updated_at,
            state_cycle: 0,
            known_approval_ids: Vec::new(),
            fallback_approval_cycle: None,
        }
    }
}
