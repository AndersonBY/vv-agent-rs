mod env;
mod execution;
mod shell_defaults;

use std::sync::Arc;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::types::{ToolArguments, ToolExecutionResult};

use execution::execute_bash_command;

pub fn run_bash_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    execute_bash_command(context, arguments)
}

pub(crate) fn bash_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "bash",
        "Run a shell command in the current workspace.",
        Arc::new(execute_bash_command),
    );
    if let Some(schema) = super::super::schemas::schema_for("bash") {
        spec.schema = schema;
    }
    spec
}
