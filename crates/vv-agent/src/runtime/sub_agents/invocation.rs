use std::cell::RefCell;

#[derive(Clone)]
pub(super) struct AssignedSubTaskIdentity {
    pub(super) task_id: String,
    pub(super) session_id: String,
}

thread_local! {
    static ASSIGNED_SUB_TASK_IDENTITY: RefCell<Option<AssignedSubTaskIdentity>> =
        const { RefCell::new(None) };
}

struct AssignedIdentityGuard(Option<AssignedSubTaskIdentity>);

impl Drop for AssignedIdentityGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        ASSIGNED_SUB_TASK_IDENTITY.with(|identity| {
            *identity.borrow_mut() = previous;
        });
    }
}

pub(crate) fn with_assigned_sub_task_identity<T>(
    task_id: String,
    session_id: String,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = ASSIGNED_SUB_TASK_IDENTITY.with(|identity| {
        identity.borrow_mut().replace(AssignedSubTaskIdentity {
            task_id,
            session_id,
        })
    });
    let _guard = AssignedIdentityGuard(previous);
    operation()
}

pub(super) fn take_assigned_sub_task_identity() -> Option<AssignedSubTaskIdentity> {
    ASSIGNED_SUB_TASK_IDENTITY.with(|identity| identity.borrow_mut().take())
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{take_assigned_sub_task_identity, with_assigned_sub_task_identity};

    fn take_pair() -> Option<(String, String)> {
        take_assigned_sub_task_identity().map(|identity| (identity.task_id, identity.session_id))
    }

    #[test]
    fn assigned_identity_is_one_shot_and_cleans_up_after_scope() {
        assert_eq!(take_pair(), None);
        with_assigned_sub_task_identity("task-one".to_string(), "session-one".to_string(), || {
            assert_eq!(
                take_pair(),
                Some(("task-one".to_string(), "session-one".to_string()))
            );
            assert_eq!(take_pair(), None);
        });
        assert_eq!(take_pair(), None);
    }

    #[test]
    fn assigned_identity_nesting_restores_only_unconsumed_outer_value() {
        with_assigned_sub_task_identity(
            "outer-task".to_string(),
            "outer-session".to_string(),
            || {
                with_assigned_sub_task_identity(
                    "inner-task".to_string(),
                    "inner-session".to_string(),
                    || {
                        assert_eq!(
                            take_pair(),
                            Some(("inner-task".to_string(), "inner-session".to_string()))
                        );
                    },
                );
                assert_eq!(
                    take_pair(),
                    Some(("outer-task".to_string(), "outer-session".to_string()))
                );
            },
        );

        with_assigned_sub_task_identity(
            "consumed-task".to_string(),
            "consumed-session".to_string(),
            || {
                assert_eq!(
                    take_pair(),
                    Some(("consumed-task".to_string(), "consumed-session".to_string(),))
                );
                with_assigned_sub_task_identity(
                    "inner-task".to_string(),
                    "inner-session".to_string(),
                    || {
                        assert_eq!(
                            take_pair(),
                            Some(("inner-task".to_string(), "inner-session".to_string()))
                        );
                    },
                );
                assert_eq!(take_pair(), None);
            },
        );
        assert_eq!(take_pair(), None);
    }

    #[test]
    fn assigned_identity_guard_cleans_up_after_panic_and_restores_outer_scope() {
        let panic = catch_unwind(|| {
            with_assigned_sub_task_identity(
                "failed-task".to_string(),
                "failed-session".to_string(),
                || panic!("scope failed"),
            );
        });
        assert!(panic.is_err());
        assert_eq!(take_pair(), None);

        with_assigned_sub_task_identity(
            "outer-task".to_string(),
            "outer-session".to_string(),
            || {
                let panic = catch_unwind(AssertUnwindSafe(|| {
                    with_assigned_sub_task_identity(
                        "inner-task".to_string(),
                        "inner-session".to_string(),
                        || panic!("inner failed"),
                    );
                }));
                assert!(panic.is_err());
                assert_eq!(
                    take_pair(),
                    Some(("outer-task".to_string(), "outer-session".to_string()))
                );
            },
        );
        assert_eq!(take_pair(), None);
    }

    #[test]
    fn assigned_identity_is_isolated_between_threads() {
        let barrier = Arc::new(Barrier::new(2));
        let workers = (0..2)
            .map(|index| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let observed = with_assigned_sub_task_identity(
                        format!("thread-task-{index}"),
                        format!("thread-session-{index}"),
                        || {
                            barrier.wait();
                            (take_pair(), take_pair())
                        },
                    );
                    (observed, take_pair())
                })
            })
            .collect::<Vec<_>>();

        for (index, worker) in workers.into_iter().enumerate() {
            let ((first, second), after_scope) = worker.join().expect("identity worker");
            assert_eq!(
                first,
                Some((
                    format!("thread-task-{index}"),
                    format!("thread-session-{index}"),
                ))
            );
            assert_eq!(second, None);
            assert_eq!(after_scope, None);
        }
        assert_eq!(take_pair(), None);
    }
}
