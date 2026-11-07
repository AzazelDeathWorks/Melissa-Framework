use melissa_core_contracts::{Module, TaskRecord, TaskState};
use proptest::prelude::*;
const STATES: [TaskState; 15] = [
    TaskState::Created,
    TaskState::Planned,
    TaskState::AwaitingApproval,
    TaskState::Queued,
    TaskState::Leased,
    TaskState::Running,
    TaskState::Verifying,
    TaskState::Repairing,
    TaskState::OutcomeUnknown,
    TaskState::NeedsReconciliation,
    TaskState::Blocked,
    TaskState::Completed,
    TaskState::Failed,
    TaskState::Cancelled,
    TaskState::TimedOut,
];
fn record(state: TaskState, version: u64) -> TaskRecord {
    TaskRecord {
        task_id: "property-task".into(),
        module: Module::Code,
        state,
        version,
        acceptance_digest: "contract".into(),
        final_artifact_digest: None,
        unknown_invocation_id: None,
    }
}
proptest! {
    #[test] fn terminal_states_cannot_restart_or_complete_again(terminal in 11usize..15,next in 0usize..15,version in 1u64..u64::MAX) {
        let mut task=record(STATES[terminal],version);let original=task.clone();prop_assert!(task.transition(version,STATES[next],None).is_err());prop_assert_eq!(task,original);
    }
    #[test] fn stale_versions_and_unknown_effects_do_not_change_state(state in 0usize..15,next in 0usize..15,version in 1u64..u64::MAX-1) {
        let mut task=record(STATES[state],version);let original=task.clone();prop_assert!(task.transition(version-1,STATES[next],None).is_err());prop_assert_eq!(&task,&original);
        task.state=TaskState::NeedsReconciliation;task.unknown_invocation_id=Some("unresolved-owned-invocation".into());let unknown=task.clone();prop_assert!(task.transition(version,STATES[next],None).is_err());prop_assert_eq!(task,unknown);
    }
}
