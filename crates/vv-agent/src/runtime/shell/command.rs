use super::{resolve_shell_invocation, PreparedShellCommand};

const WINDOWS_AUTO_CONFIRM_LINES: usize = 512;

pub fn build_shell_invocation(
    command: &str,
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<Vec<String>, String> {
    let resolved = resolve_shell_invocation(shell, windows_shell_priority)?;
    let mut invocation = resolved.prefix;
    invocation.push(command.to_string());
    Ok(invocation)
}

pub fn prepare_shell_execution(
    command: &str,
    auto_confirm: bool,
    stdin: Option<&str>,
    shell: Option<&str>,
    windows_shell_priority: Option<&[String]>,
) -> Result<PreparedShellCommand, String> {
    let resolved = resolve_shell_invocation(shell, windows_shell_priority)?;
    if !auto_confirm {
        let mut prepared = resolved.prefix.clone();
        prepared.push(command.to_string());
        return Ok(PreparedShellCommand {
            kind: resolved.kind,
            command: prepared,
            shell: Some(resolved.name),
            stdin: stdin.map(str::to_string),
        });
    }

    if resolved.kind == "bash" {
        let mut prepared = resolved.prefix.clone();
        prepared.push(format!("yes | ({command})"));
        Ok(PreparedShellCommand {
            kind: resolved.kind,
            command: prepared,
            shell: Some(resolved.name),
            stdin: stdin.map(str::to_string),
        })
    } else {
        let mut prepared = resolved.prefix.clone();
        prepared.push(command.to_string());
        Ok(PreparedShellCommand {
            kind: resolved.kind,
            command: prepared,
            shell: Some(resolved.name),
            stdin: Some(format!(
                "{}{}",
                "y\n".repeat(WINDOWS_AUTO_CONFIRM_LINES),
                stdin.unwrap_or("")
            )),
        })
    }
}
