mod args;
mod logging;
mod output;
mod task;

use std::env;
use std::sync::Arc;

use crate::config::build_vv_llm_from_local_settings;
use crate::runtime::AgentRuntime;
use crate::workspace::LocalWorkspaceBackend;

pub use self::args::{parse_cli_args_from, parse_cli_args_from_with_default_settings, CliArgs};
use self::logging::build_cli_log_handler;
pub use self::output::result_payload;
use self::task::generate_task_id;
pub use self::task::{build_cli_task, build_cli_task_from_resolved};

pub fn main() -> Result<(), String> {
    let raw_args = env::args().collect::<Vec<_>>();
    if raw_args
        .iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("{}", args::help_text());
        return Ok(());
    }
    let args = parse_cli_args_from(raw_args)?;
    let (llm, resolved) =
        build_vv_llm_from_local_settings(&args.settings_file, &args.backend, &args.model, 90.0)
            .map_err(|err| err.to_string())?;

    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(args.settings_file.clone())
        .with_default_backend(args.backend.clone());
    runtime.default_workspace = Some(args.workspace.clone());
    runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(args.workspace.clone()));
    runtime.log_handler = build_cli_log_handler(args.verbose);

    let task = build_cli_task_from_resolved(&args, &resolved, generate_task_id())?;
    let result = runtime.run(task).map_err(|err| err.to_string())?;
    let payload = result_payload(&result, &resolved);
    let output = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    println!("{output}");
    Ok(())
}
