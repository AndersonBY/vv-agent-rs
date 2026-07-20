use super::touch_type;

pub(super) fn public_export_path(id: &str) -> Option<&'static str> {
    Some(match id {
        "run_config.after_cycle_hook" => {
            export_type!(dyn vv_agent::AfterCycleHook, "vv_agent::AfterCycleHook")
        }
        "run_config.after_cycle_snapshot" => {
            export_type!(vv_agent::AfterCycleSnapshot, "vv_agent::AfterCycleSnapshot")
        }
        "run_config.after_cycle_decision" => {
            export_type!(vv_agent::AfterCycleDecision, "vv_agent::AfterCycleDecision")
        }
        "run_config.after_cycle_action" => {
            export_type!(vv_agent::AfterCycleAction, "vv_agent::AfterCycleAction")
        }
        "run_config.native_cycle_outcome" => {
            export_type!(vv_agent::NativeCycleOutcome, "vv_agent::NativeCycleOutcome")
        }
        "run_config.native_cycle_outcome_kind" => export_type!(
            vv_agent::NativeCycleOutcomeKind,
            "vv_agent::NativeCycleOutcomeKind"
        ),
        _ => return None,
    })
}
