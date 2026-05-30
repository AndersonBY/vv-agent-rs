use std::process::Command;

use crate::tools::common::{
    command_output_with_executable_busy_retry, workspace_relative_path_or_absolute,
};
use crate::workspace::{glob_match, normalized_glob_pattern};

use super::paths::normalize_rg_relative_path;
use super::types::{RgListFilesRequest, RgListFilesResult};

pub(super) fn list_files_local_rg(request: RgListFilesRequest<'_>) -> Option<RgListFilesResult> {
    let RgListFilesRequest {
        context,
        base_path,
        base_is_workspace_root,
        glob,
        include_hidden,
        include_ignored,
        ignored_root_names,
        max_results,
        scan_limit,
        rg_executable,
    } = request;

    let mut command = Command::new(rg_executable);
    command
        .arg("--files")
        .arg("--null")
        .arg("--no-messages")
        .arg("--no-ignore")
        .arg("--no-ignore-vcs");
    if include_hidden {
        command.arg("--hidden");
    }
    if !glob.trim().is_empty() && glob != "**/*" {
        command.arg("--glob").arg(glob);
    }
    if base_is_workspace_root && !include_ignored {
        for root in ignored_root_names {
            command.arg("--glob").arg(format!("!{root}/**"));
        }
    }
    let output =
        command_output_with_executable_busy_retry(command.arg(".").current_dir(base_path)).ok()?;
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return None;
    }

    let glob_pattern = normalized_glob_pattern(glob);
    let mut files = Vec::new();
    let mut matched_count = 0usize;
    let mut scanned_count = 0usize;
    let mut scan_limited = false;

    for raw_entry in output.stdout.split(|byte| *byte == b'\0') {
        if raw_entry.is_empty() {
            continue;
        }
        scanned_count += 1;
        if scanned_count > scan_limit {
            scan_limited = true;
            break;
        }
        let rel_from_base = normalize_rg_relative_path(String::from_utf8_lossy(raw_entry));
        if rel_from_base.is_empty() || !glob_match(&rel_from_base, &glob_pattern) {
            continue;
        }
        matched_count += 1;
        if files.len() < max_results {
            let output_path = workspace_relative_path_or_absolute(
                &context.workspace,
                &base_path.join(&rel_from_base),
            );
            files.push(output_path);
        }
    }

    files.sort();
    let truncated = matched_count > files.len() || scan_limited;
    Some(RgListFilesResult {
        files,
        total_count: matched_count,
        truncated,
        scan_limited,
    })
}
