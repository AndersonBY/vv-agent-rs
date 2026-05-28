use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static PROCESS_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct CapturedProcess {
    pub child: Child,
    pub output_path: PathBuf,
}

pub fn start_captured_process(
    command: &[String],
    cwd: &Path,
    stdin_text: Option<&str>,
) -> std::io::Result<CapturedProcess> {
    start_captured_process_with_env(command, cwd, stdin_text, None)
}

pub fn start_captured_process_with_env(
    command: &[String],
    cwd: &Path,
    stdin_text: Option<&str>,
    env: Option<&BTreeMap<String, String>>,
) -> std::io::Result<CapturedProcess> {
    let Some(program) = command.first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty command",
        ));
    };
    let output_path = next_output_path();
    let stdout_file = open_output_file(&output_path)?;
    let stderr_file = stdout_file.try_clone()?;

    let mut child_command = Command::new(program);
    child_command
        .args(&command[1..])
        .current_dir(cwd)
        .stdin(if stdin_text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    if let Some(env) = env {
        child_command.envs(env);
    }

    configure_process_group(&mut child_command);

    let mut child = match child_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            remove_captured_output(&output_path);
            return Err(error);
        }
    };

    if let Some(stdin_text) = stdin_text {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_text.as_bytes())?;
        }
    }

    Ok(CapturedProcess { child, output_path })
}

pub fn wait_for_child(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let started_at = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started_at.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn read_captured_output(path: &Path, limit_chars: usize) -> String {
    if limit_chars == 0 {
        return String::new();
    }
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let mut output = Vec::new();
    if file.read_to_end(&mut output).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&output)
        .chars()
        .take(limit_chars)
        .collect()
}

pub fn remove_captured_output(path: &Path) {
    let _ = fs::remove_file(path);
}

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

fn next_output_path() -> PathBuf {
    let counter = PROCESS_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "vv_agent_process_{}_{}.log",
        std::process::id(),
        counter
    ))
}

fn open_output_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(path)
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
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

#[cfg(any(windows, test))]
fn windows_hidden_process_creation_flags() -> u32 {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(windows_hidden_process_creation_flags());
}

#[cfg(all(not(unix), not(windows)))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_process_group_or_child(child: &mut Child, force: bool) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    let pid = child.id() as libc::pid_t;
    unsafe {
        if libc::kill(-pid, signal) == -1 {
            let _ = child.kill();
        }
    }
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

#[cfg(windows)]
fn kill_process_group_or_child(child: &mut Child, _force: bool) {
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
fn kill_process_group_or_child(child: &mut Child, _force: bool) {
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hidden_process_creation_flags_match_python_subprocess_defaults() {
        assert_eq!(
            windows_hidden_process_creation_flags(),
            0x0000_0200 | 0x0800_0000
        );
    }

    #[test]
    fn windows_taskkill_args_match_python_process_tree_termination() {
        assert_eq!(
            windows_taskkill_args(1234),
            vec!["/PID", "1234", "/T", "/F"]
        );
    }
}
