use super::*;
use vv_agent::checkpoint::{
    CheckpointResult, ControllerCommandReceipt, ControllerCommandWakeRecord, EventCursor,
};
use vv_agent::{
    Checkpoint, ClaimMode, HostInteractionRecoveryEnvelope, HostInteractionRecoveryResult,
};

pub(super) struct RaceStore {
    pub inner: Arc<SqliteCheckpointStore>,
    pub participant: String,
}

pub(super) fn wait_for(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "missing {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

macro_rules! forward {
    ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)*) => {
        $(
            #[allow(clippy::too_many_arguments)]
            fn $name(&self, $($arg: $ty),*) -> $result {
                self.inner.$name($($arg),*)
            }
        )*
    };
}

impl CheckpointStore for RaceStore {
    forward! {
        fn store_identity() -> String;
        fn create_checkpoint(checkpoint: Checkpoint) -> CheckpointResult<bool>;
        fn load_checkpoint(key: &str) -> CheckpointResult<Option<Checkpoint>>;
        fn claim_checkpoint(key: &str, cycle: u64, token: &str, lease: u64, now: u64, mode: ClaimMode) -> CheckpointResult<Option<Checkpoint>>;
        fn progress_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        fn suspend_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        fn commit_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        fn finalize_claimed_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        fn finalize_checkpoint(checkpoint: Checkpoint, revision: u64) -> CheckpointResult<bool>;
        fn renew_checkpoint_claim(key: &str, token: &str, lease: u64, now: u64) -> CheckpointResult<vv_agent::CheckpointRenewalOutcome>;
        fn record_event_delivery(key: &str, token: Option<&str>, revision: u64, event_id: &str, digest: &str, cursor: EventCursor) -> CheckpointResult<bool>;
        fn acknowledge_terminal(key: &str, revision: u64) -> CheckpointResult<bool>;
        fn delete_checkpoint(key: &str) -> CheckpointResult<()>;
        fn list_checkpoints() -> CheckpointResult<Vec<String>>;
        fn get_controller_command(id: &str) -> CheckpointResult<Option<ControllerCommand>>;
        fn reap_controller_command_wakes(key: &str, now: u64) -> CheckpointResult<Vec<ControllerCommandWakeRecord>>;
        fn reap_host_interaction_record(id: &str, key: &str, now: u64) -> CheckpointResult<bool>;
        fn complete_controller_command_wake(id: &str, digest: &str, token: &str, attempt: u64, outcome: &str, now: u64, error: Option<&str>) -> CheckpointResult<Option<ControllerCommandReceipt>>;
    }

    fn claim_controller_command_wake(
        &self,
        id: &str,
        digest: &str,
        token: &str,
        lease: u64,
        now: u64,
    ) -> CheckpointResult<Option<ControllerCommandReceipt>> {
        let directory = self.inner.location().parent().unwrap();
        let before = self.inner.load_checkpoint("host-tool")?.unwrap();
        assert!(before.claim_token.is_none());
        std::fs::write(
            directory.join(format!("ready-{}", self.participant)),
            b"ready",
        )
        .unwrap();
        wait_for(&directory.join("release-claim"));
        if self.participant == "race_right" && directory.join("race-after-wake").exists() {
            wait_for(&directory.join("release-loser-claim"));
        }
        self.inner
            .claim_controller_command_wake(id, digest, token, lease, now)
    }

    fn claim_and_consume_host_interaction_response(
        &self,
        envelope: HostInteractionRecoveryEnvelope,
    ) -> CheckpointResult<HostInteractionRecoveryResult> {
        let result = self
            .inner
            .claim_and_consume_host_interaction_response(envelope)?;
        let directory = self.inner.location().parent().unwrap();
        if result.kind == "applied" && !directory.join("race-after-wake").exists() {
            let checkpoint = self.inner.load_checkpoint("host-tool")?.unwrap();
            std::fs::write(
                directory.join("admitted.json"),
                serde_json::to_vec(&vv_agent::runtime::checkpoint_codec::checkpoint_to_value(
                    &checkpoint,
                    vv_agent::checkpoint::MAX_WIRE_INTEGER,
                )?)
                .unwrap(),
            )
            .unwrap();
            std::fs::write(directory.join("admitted"), b"ready").unwrap();
            wait_for(&directory.join("release-owner"));
        }
        Ok(result)
    }
}
