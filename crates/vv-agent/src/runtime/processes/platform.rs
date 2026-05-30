use std::process::{Child, Command};

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
pub(super) fn kill_process_group_or_child(child: &mut Child, force: bool) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    let pid = child.id() as libc::pid_t;
    unsafe {
        if libc::kill(-pid, signal) == -1 {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
pub(super) fn kill_process_group_or_child(child: &mut Child, _force: bool) {
    use std::process::Stdio;

    let taskkill = Command::new("taskkill")
        .args(windows_taskkill_args(child.id()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !taskkill.map(|status| status.success()).unwrap_or(false) {
        let _ = child.kill();
    }
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn kill_process_group_or_child(child: &mut Child, _force: bool) {
    let _ = child.kill();
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
