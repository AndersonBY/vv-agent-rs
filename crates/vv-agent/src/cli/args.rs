use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub prompt: String,
    pub backend: String,
    pub model: String,
    pub settings_file: PathBuf,
    pub workspace: PathBuf,
    pub max_cycles: u32,
    pub language: String,
    pub agent_type: Option<String>,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Run(CliArgs),
    AppServer(AppServerCliCommand),
    Debug(DebugCliCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppServerCliCommand {
    ListenStdio,
    GenerateTs { out: PathBuf },
    GenerateJsonSchema { out: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugCliCommand {
    AppServerSendMessage { message: String },
}

pub fn parse_cli_args_from<I, S>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let default_settings =
        env::var("VV_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.json".to_string());
    parse_cli_args_from_with_default_settings(args, default_settings)
}

pub fn parse_cli_command_from<I, S>(args: I) -> Result<CliCommand, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let default_settings =
        env::var("VV_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.json".to_string());
    parse_cli_command_from_with_default_settings(args, default_settings)
}

pub fn parse_cli_command_from_with_default_settings<I, S>(
    args: I,
    default_settings: impl Into<String>,
) -> Result<CliCommand, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut values = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if !values.is_empty() {
        values.remove(0);
    }
    match values.first().map(String::as_str) {
        Some("app-server") => parse_app_server_command(&values[1..]).map(CliCommand::AppServer),
        Some("debug") => parse_debug_command(&values[1..]).map(CliCommand::Debug),
        _ => parse_cli_args_from_with_default_settings(
            std::iter::once("vv-agent".to_string()).chain(values),
            default_settings,
        )
        .map(CliCommand::Run),
    }
}

pub fn parse_cli_args_from_with_default_settings<I, S>(
    args: I,
    default_settings: impl Into<String>,
) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut values = args.into_iter().map(Into::into).collect::<Vec<_>>();
    if !values.is_empty() {
        values.remove(0);
    }

    let mut parsed = ParsedCliArgs::with_default_settings(default_settings.into());
    parsed.consume(values)?;
    parsed.finish()
}

fn parse_app_server_command(values: &[String]) -> Result<AppServerCliCommand, String> {
    match values.first().map(String::as_str) {
        Some("--listen") => {
            let listen = values
                .get(1)
                .ok_or_else(|| "app-server --listen requires a value".to_string())?;
            if listen != "stdio" {
                return Err("only app-server --listen stdio is supported".to_string());
            }
            Ok(AppServerCliCommand::ListenStdio)
        }
        Some("generate-ts") => Ok(AppServerCliCommand::GenerateTs {
            out: parse_out_dir(&values[1..], "app-server generate-ts")?,
        }),
        Some("generate-json-schema") => Ok(AppServerCliCommand::GenerateJsonSchema {
            out: parse_out_dir(&values[1..], "app-server generate-json-schema")?,
        }),
        _ => Err(format!("unknown app-server command\n\n{}", help_text())),
    }
}

fn parse_debug_command(values: &[String]) -> Result<DebugCliCommand, String> {
    match (
        values.first().map(String::as_str),
        values.get(1).map(String::as_str),
        values.get(2).map(String::as_str),
    ) {
        (Some("app-server"), Some("send-message"), Some(message)) => {
            Ok(DebugCliCommand::AppServerSendMessage {
                message: message.to_string(),
            })
        }
        _ => Err(format!("unknown debug command\n\n{}", help_text())),
    }
}

fn parse_out_dir(values: &[String], command: &str) -> Result<PathBuf, String> {
    if values.first().map(String::as_str) != Some("--out") {
        return Err(format!("{command} requires --out <dir>"));
    }
    values
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{command} requires --out <dir>"))
}

struct ParsedCliArgs {
    prompt: Option<String>,
    backend: String,
    model: String,
    settings_file: PathBuf,
    workspace: PathBuf,
    max_cycles: u32,
    language: String,
    agent_type: Option<String>,
    verbose: bool,
}

impl ParsedCliArgs {
    fn with_default_settings(default_settings: String) -> Self {
        Self {
            prompt: None,
            backend: "moonshot".to_string(),
            model: "kimi-k2.6".to_string(),
            settings_file: PathBuf::from(default_settings),
            workspace: PathBuf::from("./workspace"),
            max_cycles: 80,
            language: "zh-CN".to_string(),
            agent_type: None,
            verbose: false,
        }
    }

    fn consume(&mut self, values: Vec<String>) -> Result<(), String> {
        let mut index = 0;
        while index < values.len() {
            let flag = &values[index];
            index += 1;
            match flag.as_str() {
                "--prompt" => self.prompt = Some(next_value(&values, &mut index, "--prompt")?),
                "--backend" => self.backend = next_value(&values, &mut index, "--backend")?,
                "--model" => self.model = next_value(&values, &mut index, "--model")?,
                "--settings-file" => {
                    self.settings_file =
                        PathBuf::from(next_value(&values, &mut index, "--settings-file")?)
                }
                "--workspace" => {
                    self.workspace = PathBuf::from(next_value(&values, &mut index, "--workspace")?)
                }
                "--max-cycles" => {
                    let raw = next_value(&values, &mut index, "--max-cycles")?;
                    self.max_cycles = raw
                        .parse::<u32>()
                        .map_err(|_| "--max-cycles must be an integer".to_string())?
                        .max(1);
                }
                "--language" => self.language = next_value(&values, &mut index, "--language")?,
                "--agent-type" => {
                    self.agent_type = Some(next_value(&values, &mut index, "--agent-type")?)
                        .filter(|value| !value.trim().is_empty())
                }
                "--verbose" => self.verbose = true,
                "--help" | "-h" => return Err(help_text()),
                other => return Err(format!("unknown argument: {other}\n\n{}", help_text())),
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<CliArgs, String> {
        let Some(prompt) = self.prompt.filter(|value| !value.trim().is_empty()) else {
            return Err(format!("--prompt is required\n\n{}", help_text()));
        };

        Ok(CliArgs {
            prompt,
            backend: self.backend,
            model: self.model,
            settings_file: self.settings_file,
            workspace: self.workspace,
            max_cycles: self.max_cycles,
            language: self.language,
            agent_type: self.agent_type,
            verbose: self.verbose,
        })
    }
}

fn next_value(values: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    let Some(value) = values.get(*index) else {
        return Err(format!("{flag} requires a value"));
    };
    *index += 1;
    Ok(value.clone())
}

pub(crate) fn help_text() -> String {
    [
        "Run a vv-agent task against a configured LLM endpoint.",
        "",
        "Required:",
        "  --prompt <text>",
        "",
        "Options:",
        "  --backend <key>        Provider backend key in LLM_SETTINGS (default: moonshot)",
        "  --model <key>          Model key in provider models (default: kimi-k2.6)",
        "  --settings-file <path> Path to local settings (default: VV_AGENT_LOCAL_SETTINGS or local_settings.json)",
        "  --workspace <path>     Workspace directory (default: ./workspace)",
        "  --max-cycles <n>       Max runtime cycles (default: 80)",
        "  --language <locale>    System prompt language (default: zh-CN)",
        "  --agent-type <type>    Agent type, e.g. computer",
        "  --verbose             Show per-cycle runtime logs",
    ]
    .join("\n")
}
