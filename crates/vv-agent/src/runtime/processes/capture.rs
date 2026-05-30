use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::output::{next_output_path, open_output_file, remove_captured_output};
use super::platform::configure_process_group;

#[derive(Debug)]
pub struct CapturedProcess {
    pub child: Child,
    pub output_path: std::path::PathBuf,
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
