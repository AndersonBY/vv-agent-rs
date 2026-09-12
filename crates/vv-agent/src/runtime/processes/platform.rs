use super::ManagedChild;
use std::process::Command;

#[cfg(unix)]
pub(super) fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
pub(super) fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(windows_hidden_process_creation_flags());
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
pub(super) fn kill_process_group_or_child(
    child: &mut ManagedChild,
    force: bool,
) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(tree) = child.tree.as_mut() {
        return tree.request_stop(force);
    }
    if child.try_wait()?.is_some() {
        return Err(std::io::Error::other(
            "cannot signal an untracked, reaped process group",
        ));
    }
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    let pid = child.id() as libc::pid_t;
    unsafe {
        if libc::kill(-pid, signal) == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn kill_process_group_or_child(
    child: &mut ManagedChild,
    _force: bool,
) -> std::io::Result<()> {
    if child.try_wait()?.is_some() {
        return Err(std::io::Error::other(
            "cannot signal an untracked, reaped process tree",
        ));
    }
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::time::Duration;

    let pids = windows_tree_pids(child.id())?;
    if pids.is_empty() {
        return Ok(());
    }
    let mut command = Command::new("taskkill");
    for pid in pids {
        command.args(["/PID", &pid.to_string()]);
    }
    let mut taskkill = command
        .args(["/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(windows_hidden_process_creation_flags())
        .spawn()?;
    if super::capture::wait_for_child(&mut taskkill, Duration::from_secs(1))?.is_none() {
        let _ = taskkill.kill();
        let _ = taskkill.wait();
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "process-tree stop is unconfirmed",
        ));
    }
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn kill_process_group_or_child(
    _child: &mut ManagedChild,
    _force: bool,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process-tree stop is unavailable",
    ))
}

pub(crate) fn process_tree_is_running(child: &mut ManagedChild) -> std::io::Result<bool> {
    if child.try_wait()?.is_none() {
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    if let Some(tree) = child.tree.as_mut() {
        return tree.confirmed().map(|complete| !complete);
    }
    // Parent/group absence cannot exclude escaped descendants. In particular,
    // no macOS or Windows snapshot is treated as a complete-tree proof.
    Err(std::io::Error::other(
        "complete process-tree observation unavailable for an unsupervised child",
    ))
}

#[cfg(windows)]
fn windows_tree_pids(root: u32) -> std::io::Result<std::collections::BTreeSet<u32>> {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::c_void;
    #[repr(C)]
    struct ProcessEntry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        exe: [u16; 260],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let snapshot = unsafe { CreateToolhelp32Snapshot(2, 0) };
    if snapshot as isize == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let result = (|| {
        let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
        entry.size = std::mem::size_of::<ProcessEntry>() as u32;
        let mut parents = BTreeMap::new();
        let mut found = unsafe { Process32FirstW(snapshot, &mut entry) };
        while found != 0 {
            parents.insert(entry.pid, entry.parent);
            found = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(18) {
            return Err(error);
        }
        let mut family = BTreeSet::from([root]);
        loop {
            let descendants: BTreeSet<_> = parents
                .iter()
                .filter_map(|(pid, parent)| family.contains(parent).then_some(*pid))
                .collect();
            if descendants.is_subset(&family) {
                family.retain(|pid| parents.contains_key(pid));
                return Ok(family);
            }
            family.extend(descendants);
        }
    })();
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

#[cfg(any(windows, test))]
fn windows_hidden_process_creation_flags() -> u32 {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
}

#[cfg(any(windows, test))]
fn windows_taskkill_args(pid: u32) -> Vec<String> {
    vec![
        "/PID".to_string(),
        pid.to_string(),
        "/T".to_string(),
        "/F".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hidden_process_creation_flags_match_agent_subprocess_defaults() {
        assert_eq!(
            windows_hidden_process_creation_flags(),
            0x0000_0200 | 0x0800_0000
        );
    }

    #[test]
    fn windows_taskkill_args_match_agent_process_tree_termination() {
        assert_eq!(
            windows_taskkill_args(1234),
            vec!["/PID", "1234", "/T", "/F"]
        );
    }
}
