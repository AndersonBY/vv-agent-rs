mod capture;
mod output;
mod platform;
mod termination;

pub use capture::{
    start_captured_process, start_captured_process_with_env, wait_for_child, CapturedProcess,
};
pub use output::{read_captured_output, remove_captured_output};
pub use termination::kill_process_tree;
