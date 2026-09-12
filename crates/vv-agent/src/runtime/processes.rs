mod capture;
mod child;
mod output;
mod platform;
#[cfg(target_os = "linux")]
mod supervisor;
mod termination;

pub(crate) use capture::observed_exit_code;
pub use capture::{
    start_captured_process, start_captured_process_with_env, wait_for_child, CapturedProcess,
};
pub use child::ManagedChild;
pub use output::{read_captured_output, remove_captured_output};
pub(crate) use platform::process_tree_is_running;
pub use termination::kill_process_tree;
