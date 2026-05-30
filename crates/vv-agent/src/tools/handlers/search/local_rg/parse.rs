use std::path::Path;

use serde_json::Value;

use crate::tools::base::ToolContext;

mod decode;
mod events;
mod paths;
mod state;

use self::events::RgJsonEvent;
use self::state::RgJsonState;
use super::types::RgGrepResult;

pub(super) fn parse_rg_json_output(
    context: &ToolContext,
    base_path: &Path,
    output_mode: &str,
    file_type: Option<&str>,
    multiline: bool,
    stdout: &[u8],
) -> Option<RgGrepResult> {
    let mut state = RgJsonState::new(output_mode, multiline);

    let stdout = String::from_utf8_lossy(stdout);
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).ok()?;
        if let Some(event) = RgJsonEvent::from_value(context, base_path, file_type, &event) {
            state.record(event);
        }
    }

    Some(state.finish())
}
