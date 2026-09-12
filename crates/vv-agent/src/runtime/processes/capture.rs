use std::collections::BTreeMap;
use std::io::{Seek, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::output::{next_output_path, open_output_file, remove_captured_output};
use super::platform::configure_process_group;
use super::ManagedChild;

#[derive(Debug)]
pub struct CapturedProcess {
    pub child: ManagedChild,
    pub output_path: std::path::PathBuf,
    pub started_at: Instant,
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
    let input_path = stdin_text.map(|_| next_output_path().with_extension("stdin"));
    let started = (|| {
        let stdout_file = open_output_file(&output_path)?;
        let stderr_file = stdout_file.try_clone()?;
        // A command that does not read stdin cannot block yield or the watchdog.
        let stdin = if let (Some(text), Some(path)) = (stdin_text, input_path.as_ref()) {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)?;
            file.write_all(text.as_bytes())?;
            file.rewind()?;
            Stdio::from(file)
        } else {
            Stdio::null()
        };
        let mut child_command = Command::new(program);
        child_command
            .args(&command[1..])
            .current_dir(cwd)
            .stdin(stdin)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        if let Some(env) = env {
            child_command.envs(env);
        }
        configure_process_group(&mut child_command);
        let started_at = Instant::now();
        ManagedChild::spawn(&mut child_command).map(|child| CapturedProcess {
            child,
            output_path: output_path.clone(),
            started_at,
        })
    })();
    if let Some(path) = input_path {
        remove_captured_output(&path);
    }
    if started.is_err() {
        remove_captured_output(&output_path);
    }
    started
}

pub(crate) fn observed_exit_code(status: ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .or_else(|| status.signal().map(|signal| -signal))
    }
    #[cfg(not(unix))]
    status.code()
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
