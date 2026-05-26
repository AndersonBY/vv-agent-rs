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
    let mut output = String::new();
    if file.read_to_string(&mut output).is_err() {
        return String::new();
    }
    output.chars().take(limit_chars).collect()
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

#[cfg(not(unix))]
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

#[cfg(not(unix))]
fn kill_process_group_or_child(child: &mut Child, _force: bool) {
    let _ = child.kill();
}
