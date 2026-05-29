use std::io;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

pub(crate) fn command_output_with_executable_busy_retry(
    command: &mut Command,
) -> io::Result<Output> {
    const MAX_ATTEMPTS: usize = 3;

    for attempt in 0..MAX_ATTEMPTS {
        match command.output() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < MAX_ATTEMPTS =>
            {
                thread::sleep(Duration::from_millis(10 * (attempt as u64 + 1)));
            }
            result => return result,
        }
    }

    unreachable!("command output retry loop always returns");
}
