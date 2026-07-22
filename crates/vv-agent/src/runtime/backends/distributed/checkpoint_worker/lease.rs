//! Distributed checkpoint lease renewal and heartbeat handling.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_checkpoint_lease<T>(
    store: Arc<dyn CheckpointStore>,
    checkpoint_key: &str,
    cycle_index: u64,
    lease_duration_ms: u64,
    deadline_unix_ms: Option<u64>,
    claim_token: &str,
    operation: impl FnOnce(&LeaseHeartbeatStatus) -> LeaseOperationResult<T>,
) -> Result<T, String> {
    let stopped = Arc::new((Mutex::new(false), Condvar::new()));
    let heartbeat_status = LeaseHeartbeatStatus::new();
    let checkpoint_key = checkpoint_key.to_string();
    let claim_token = claim_token.to_string();
    let initial_checkpoint = load_checkpoint(store.as_ref(), &checkpoint_key)?;
    if initial_checkpoint.claim_token.as_deref() != Some(claim_token.as_str())
        || initial_checkpoint.claimed_cycle != Some(cycle_index)
    {
        return Err("checkpoint lease heartbeat failed: claim is no longer active".to_string());
    }
    let known_expiry = initial_checkpoint.lease_expires_at_ms.ok_or_else(|| {
        "checkpoint lease heartbeat failed: claim is no longer active".to_string()
    })?;
    let initial = renew_checkpoint_lease(
        store.as_ref(),
        &checkpoint_key,
        &claim_token,
        lease_duration_ms,
        deadline_unix_ms,
        known_expiry,
    )
    .map_err(|failure| format!("checkpoint lease heartbeat failed: {}", failure.message))?;

    let result = std::thread::scope(|scope| {
        let stopped_for_thread = stopped.clone();
        let status_for_thread = heartbeat_status.clone();
        let store_for_thread = store.clone();
        let checkpoint_key_for_thread = checkpoint_key.clone();
        let claim_token_for_thread = claim_token.clone();
        let heartbeat = scope.spawn(move || {
            let mut known_expiry = initial.lease_expires_at_ms;
            let mut interval = lease_heartbeat_interval(initial.effective_lease_ms);
            loop {
                let (lock, changed) = &*stopped_for_thread;
                let guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (guard, _) = changed
                    .wait_timeout_while(guard, interval, |stopped| !*stopped)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *guard {
                    break;
                }
                drop(guard);
                let phase = status_for_thread.commit_phase();
                match renew_checkpoint_lease(
                    store_for_thread.as_ref(),
                    &checkpoint_key_for_thread,
                    &claim_token_for_thread,
                    lease_duration_ms,
                    deadline_unix_ms,
                    known_expiry,
                ) {
                    Ok(renewal) => {
                        known_expiry = renewal.lease_expires_at_ms;
                        interval = lease_heartbeat_interval(renewal.effective_lease_ms);
                    }
                    Err(failure) => {
                        status_for_thread.record(failure, phase);
                        break;
                    }
                }
            }
        });

        let stop_guard = LeaseHeartbeatStopGuard::new(stopped.clone());
        let result = operation(&heartbeat_status);
        drop(stop_guard);
        heartbeat
            .join()
            .map_err(|_| "checkpoint v2 lease heartbeat panicked".to_string())?;
        Ok::<_, String>(result)
    })?;

    let (failure, commit_phase) = heartbeat_status.take();
    if let Some(failure) = failure {
        let commit_consumed_claim = result.claim_committed
            && commit_phase == LeaseCommitPhase::Succeeded
            && failure.renewal_started_during_commit
            && failure.renewal.kind == LeaseRenewalFailureKind::ActiveClaimLost;
        if !commit_consumed_claim {
            return Err(format!(
                "checkpoint lease heartbeat failed: {}",
                failure.renewal.message
            ));
        }
    }
    Ok(result.value)
}

pub(super) fn renew_checkpoint_lease(
    store: &dyn CheckpointStore,
    checkpoint_key: &str,
    claim_token: &str,
    lease_duration_ms: u64,
    deadline_unix_ms: Option<u64>,
    known_expiry: u64,
) -> Result<LeaseRenewal, LeaseRenewalFailure> {
    let now_ms = now_unix_ms().map_err(LeaseRenewalFailure::coordination)?;
    if deadline_unix_ms.is_some_and(|deadline| deadline <= now_ms) {
        return Err(LeaseRenewalFailure::coordination(format!(
            "distributed job deadline expired while renewing {checkpoint_key}"
        )));
    }
    let lease_expires_at_ms = lease_expiry_at(now_ms, lease_duration_ms, deadline_unix_ms)
        .map_err(LeaseRenewalFailure::coordination)?;
    let renewed = store
        .renew_checkpoint_claim(checkpoint_key, claim_token, lease_expires_at_ms, now_ms)
        .map_err(|error| LeaseRenewalFailure::coordination(error.to_string()))?;
    let observed_at_ms = now_unix_ms().map_err(LeaseRenewalFailure::coordination)?;
    if !renewed {
        return Err(
            if observed_at_ms >= known_expiry || observed_at_ms >= lease_expires_at_ms {
                LeaseRenewalFailure::claim_lease_expired()
            } else {
                LeaseRenewalFailure::active_claim_lost()
            },
        );
    }
    if observed_at_ms >= known_expiry || observed_at_ms >= lease_expires_at_ms {
        return Err(LeaseRenewalFailure::claim_lease_expired());
    }
    Ok(LeaseRenewal {
        lease_expires_at_ms,
        effective_lease_ms: lease_expires_at_ms - observed_at_ms,
    })
}

pub(super) fn lease_heartbeat_interval(lease_duration_ms: u64) -> Duration {
    let interval_micros = lease_duration_ms
        .saturating_mul(1_000)
        .saturating_div(3)
        .clamp(1, 30_000_000);
    Duration::from_micros(interval_micros)
}
