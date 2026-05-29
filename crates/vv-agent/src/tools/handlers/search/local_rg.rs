mod command;
mod parse;
mod paths;
#[cfg(all(test, unix))]
mod tests;
mod types;

use std::process::Command;

use crate::tools::common::command_output_with_executable_busy_retry;

use command::rg_file_type_globs;
use parse::parse_rg_json_output;
use paths::local_ignored_root_names;

pub(super) use command::resolve_rg_executable;
pub(super) use paths::is_workspace_root_path;
pub(super) use types::{RgGrepResult, RgWorkspaceGrepRequest};

pub(super) fn workspace_grep_local_rg(request: RgWorkspaceGrepRequest<'_>) -> Option<RgGrepResult> {
    let RgWorkspaceGrepRequest {
        context,
        path,
        glob_pattern,
        pattern,
        output_mode,
        file_type,
        case_insensitive,
        multiline,
        before_context,
        after_context,
        include_hidden,
        include_ignored,
        rg_executable,
    } = request;

    let base_path = context.resolve_workspace_path(path).ok()?;
    if !base_path.exists() || !base_path.is_dir() {
        return None;
    }

    let base_is_workspace_root = is_workspace_root_path(path);
    let ignored_root_names = if base_is_workspace_root && !include_ignored {
        local_ignored_root_names(&base_path)
    } else {
        Vec::new()
    };

    let mut command = Command::new(rg_executable);
    command
        .arg("--json")
        .arg("--line-number")
        .arg("--color")
        .arg("never")
        .arg("--no-messages");
    if include_hidden {
        command.arg("--hidden");
    }
    if include_ignored {
        command.arg("--no-ignore").arg("--no-ignore-vcs");
    }
    if case_insensitive {
        command.arg("-i");
    }
    if multiline {
        command.arg("--multiline").arg("--multiline-dotall");
    }
    if before_context > 0 {
        command
            .arg("--before-context")
            .arg(before_context.to_string());
    }
    if after_context > 0 {
        command
            .arg("--after-context")
            .arg(after_context.to_string());
    }
    if !glob_pattern.trim().is_empty() && glob_pattern != "**/*" {
        command.arg("--glob").arg(glob_pattern);
    }
    if base_is_workspace_root && !include_ignored {
        for root in &ignored_root_names {
            command.arg("--glob").arg(format!("!{root}/**"));
        }
    }
    if let Some(file_type) = file_type {
        for file_glob in rg_file_type_globs(file_type) {
            command.arg("--iglob").arg(file_glob);
        }
    }
    let output = command_output_with_executable_busy_retry(
        command
            .arg("--regexp")
            .arg(pattern)
            .arg(".")
            .current_dir(&base_path),
    )
    .ok()?;
    if !matches!(output.status.code(), Some(0) | Some(1) | Some(2)) {
        return None;
    }

    parse_rg_json_output(
        context,
        &base_path,
        output_mode,
        file_type,
        multiline,
        &output.stdout,
    )
}
