use std::time::{Duration, Instant};

use super::platform::{kill_process_group_or_child, process_tree_is_running};
use super::ManagedChild;

pub fn kill_process_tree(child: &mut ManagedChild) -> bool {
    for force in [false, true] {
        if matches!(process_tree_is_running(child), Ok(false)) {
            return true;
        }
        // A failed signal write may race the supervisor's completion proof.
        let _ = kill_process_group_or_child(child, force);
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if matches!(child.try_wait(), Ok(Some(_)))
                && matches!(process_tree_is_running(child), Ok(false))
            {
                return true;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    false
}
