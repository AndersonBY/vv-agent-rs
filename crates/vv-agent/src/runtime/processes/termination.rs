use std::process::Child;
use std::time::Duration;

use super::capture::wait_for_child;
use super::platform::kill_process_group_or_child;

pub fn kill_process_tree(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    kill_process_group_or_child(child, false);
    if wait_for_child(child, Duration::from_millis(500))
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    kill_process_group_or_child(child, true);
    let _ = wait_for_child(child, Duration::from_millis(500));
}
