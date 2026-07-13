use std::env;
use std::path::PathBuf;

use crate::model_settings::ModelSettings;

#[derive(Debug, Clone, PartialEq)]
pub struct CliArgs {
    pub prompt: String,
    pub backend: String,
    pub model: String,
    pub settings_file: PathBuf,
    pub workspace: PathBuf,
    pub max_cycles: u32,
    pub language: String,
    pub agent_type: Option<String>,
    pub model_settings: Option<ModelSettings>,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // Preserve the public command shape without boxed variants.
pub enum CliCommand {
    Run(CliArgs),
    AppServer(AppServerCliCommand),
    Debug(DebugCliCommand),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppServerCliCommand {
    ListenStdio {
        settings_file: PathBuf,
        backend: String,
        model: String,
        timeout_seconds: f64,
    },
    GenerateTs {
        out: PathBuf,
    },
    GenerateJsonSchema {
        out: PathBuf,
    },
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
    let default_settings = default_settings_file_from_environment();
    parse_cli_args_from_with_default_settings(args, default_settings)
}

pub fn parse_cli_command_from<I, S>(args: I) -> Result<CliCommand, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let default_settings = default_settings_file_from_environment();
    parse_cli_command_from_with_default_settings(args, default_settings)
}

fn default_settings_file_from_environment() -> String {
    non_blank_environment_value("VV_AGENT_LOCAL_SETTINGS")
        .or_else(|| non_blank_environment_value("V_AGENT_LOCAL_SETTINGS"))
        .unwrap_or_else(|| "local_settings.json".to_string())
}

fn non_blank_environment_value(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
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
        Some("generate-ts") => Ok(AppServerCliCommand::GenerateTs {
            out: parse_out_dir(&values[1..], "app-server generate-ts")?,
        }),
        Some("generate-json-schema" | "schema") => Ok(AppServerCliCommand::GenerateJsonSchema {
            out: parse_out_dir(&values[1..], "app-server generate-json-schema")?,
        }),
        _ => parse_app_server_listener(values),
    }
}

fn parse_app_server_listener(values: &[String]) -> Result<AppServerCliCommand, String> {
    let values = normalize_equals_arguments(values.to_vec());
    let mut parsed = ParsedAppServerListener::default();
    parsed.consume(&values)?;
    parsed.finish()
}

#[derive(Default)]
struct ParsedAppServerListener {
    listen: Option<String>,
    settings_file: Option<PathBuf>,
    backend: Option<String>,
    model: Option<String>,
    timeout_seconds: Option<f64>,
}

impl ParsedAppServerListener {
    fn consume(&mut self, values: &[String]) -> Result<(), String> {
        let mut index = 0;
        while index < values.len() {
            let flag = &values[index];
            index += 1;
            match flag.as_str() {
                "--listen" => {
                    reject_duplicate(self.listen.is_some(), flag)?;
                    let listen = next_non_blank_value(values, &mut index, flag)?;
                    if listen != "stdio" {
                        return Err("only app-server --listen stdio is supported".to_string());
                    }
                    self.listen = Some(listen);
                }
                "--settings" => {
                    reject_duplicate(self.settings_file.is_some(), flag)?;
                    self.settings_file = Some(PathBuf::from(next_non_blank_value(
                        values, &mut index, flag,
                    )?));
                }
                "--backend" => {
                    reject_duplicate(self.backend.is_some(), flag)?;
                    self.backend = Some(next_non_blank_value(values, &mut index, flag)?);
                }
                "--model" => {
                    reject_duplicate(self.model.is_some(), flag)?;
                    self.model = Some(next_non_blank_value(values, &mut index, flag)?);
                }
                "--timeout-seconds" => {
                    reject_duplicate(self.timeout_seconds.is_some(), flag)?;
                    let raw = next_non_blank_value(values, &mut index, flag)?;
                    self.timeout_seconds = Some(parse_positive_f64(&raw, flag)?);
                }
                other => return Err(format!("unknown app-server argument: {other}")),
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<AppServerCliCommand, String> {
        let mut missing = Vec::new();
        if self.listen.is_none() {
            missing.push("--listen");
        }
        if self.settings_file.is_none() {
            missing.push("--settings");
        }
        if self.backend.is_none() {
            missing.push("--backend");
        }
        if self.model.is_none() {
            missing.push("--model");
        }
        if !missing.is_empty() {
            return Err(format!("app-server requires {}", missing.join(", ")));
        }

        Ok(AppServerCliCommand::ListenStdio {
            settings_file: self.settings_file.expect("checked above"),
            backend: self.backend.expect("checked above"),
            model: self.model.expect("checked above"),
            timeout_seconds: self.timeout_seconds.unwrap_or(90.0),
        })
    }
}

fn reject_duplicate(seen: bool, flag: &str) -> Result<(), String> {
    if seen {
        return Err(format!("duplicate app-server argument: {flag}"));
    }
    Ok(())
}

fn next_non_blank_value(
    values: &[String],
    index: &mut usize,
    flag: &str,
) -> Result<String, String> {
    let value = next_value(values, index, flag)?;
    if value.trim().is_empty() {
        return Err(format!("{flag} requires a value"));
    }
    Ok(value)
}

fn parse_debug_command(values: &[String]) -> Result<DebugCliCommand, String> {
    if values.first().map(String::as_str) == Some("app-server")
        && values.get(1).map(String::as_str) == Some("send-message")
        && values.len() >= 3
    {
        return Ok(DebugCliCommand::AppServerSendMessage {
            message: values[2..].join(" "),
        });
    }
    Err(format!("unknown debug command\n\n{}", help_text()))
}

fn parse_out_dir(values: &[String], command: &str) -> Result<PathBuf, String> {
    let values = normalize_equals_arguments(values.to_vec());
    if values.first().map(String::as_str) != Some("--out") || values.len() != 2 {
        return Err(format!("{command} requires --out <dir>"));
    }
    let out = values.get(1).expect("length checked above");
    if out.trim().is_empty() || cli_flag(out) {
        return Err(format!("{command} requires --out <dir>"));
    }
    Ok(PathBuf::from(out))
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
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_tokens: Option<u32>,
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
            temperature: None,
            top_p: None,
            max_tokens: None,
            verbose: false,
        }
    }

    fn consume(&mut self, values: Vec<String>) -> Result<(), String> {
        let values = normalize_equals_arguments(values);
        let mut index = 0;
        while index < values.len() {
            let flag = &values[index];
            index += 1;
            match flag.as_str() {
                "--prompt" => self.prompt = Some(next_prompt(&values, &mut index)?),
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
                "--temperature" => {
                    let raw = next_value(&values, &mut index, "--temperature")?;
                    self.temperature = Some(parse_temperature(&raw)?);
                }
                "--top-p" => {
                    let raw = next_value(&values, &mut index, "--top-p")?;
                    self.top_p = Some(parse_top_p(&raw)?);
                }
                "--max-tokens" => {
                    let raw = next_value(&values, &mut index, "--max-tokens")?;
                    self.max_tokens = Some(parse_positive_u32(&raw, "--max-tokens")?);
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
        let model_settings =
            if self.temperature.is_some() || self.top_p.is_some() || self.max_tokens.is_some() {
                let settings = ModelSettings {
                    temperature: self.temperature,
                    top_p: self.top_p,
                    max_tokens: self.max_tokens,
                    ..ModelSettings::default()
                };
                settings.validate()?;
                Some(settings)
            } else {
                None
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
            model_settings,
            verbose: self.verbose,
        })
    }
}

fn normalize_equals_arguments(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let Some((flag, argument)) = value.split_once('=') else {
            normalized.push(value);
            continue;
        };
        if value_flag(flag) {
            normalized.push(flag.to_string());
            normalized.push(argument.to_string());
        } else {
            normalized.push(value);
        }
    }
    normalized
}

fn next_prompt(values: &[String], index: &mut usize) -> Result<String, String> {
    let start = *index;
    while *index < values.len() && !cli_flag(&values[*index]) {
        *index += 1;
    }
    if *index == start {
        return Err("--prompt requires a value".to_string());
    }
    Ok(values[start..*index].join(" "))
}

fn cli_flag(value: &str) -> bool {
    matches!(value, "--verbose" | "--help" | "-h") || value_flag(value) || value.starts_with("--")
}

fn value_flag(value: &str) -> bool {
    matches!(
        value,
        "--listen"
            | "--settings"
            | "--timeout-seconds"
            | "--prompt"
            | "--backend"
            | "--model"
            | "--settings-file"
            | "--workspace"
            | "--max-cycles"
            | "--language"
            | "--agent-type"
            | "--temperature"
            | "--top-p"
            | "--max-tokens"
    )
}

fn parse_temperature(value: &str) -> Result<f64, String> {
    let parsed = parse_f64(value, "--temperature")?;
    if parsed < 0.0 {
        return Err("--temperature must be a finite number at least 0".to_string());
    }
    Ok(parsed)
}

fn parse_top_p(value: &str) -> Result<f64, String> {
    let parsed = parse_f64(value, "--top-p")?;
    if !(0.0..=1.0).contains(&parsed) {
        return Err("--top-p must be a finite number between 0 and 1".to_string());
    }
    Ok(parsed)
}

fn parse_f64(value: &str, flag: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("{flag} must be a finite number"))?;
    if !parsed.is_finite() {
        return Err(format!("{flag} must be a finite number"));
    }
    Ok(parsed)
}

fn parse_positive_f64(value: &str, flag: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("{flag} must be a finite positive number"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(format!("{flag} must be a finite positive number"));
    }
    Ok(parsed)
}

fn parse_positive_u32(value: &str, flag: &str) -> Result<u32, String> {
    let parsed = value
        .parse::<i128>()
        .map_err(|_| format!("{flag} must be an integer"))?;
    if !(1..=i128::from(u32::MAX)).contains(&parsed) {
        return Err(format!("{flag} must be between 1 and {}", u32::MAX));
    }
    Ok(parsed as u32)
}

fn next_value(values: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    let Some(value) = values.get(*index) else {
        return Err(format!("{flag} requires a value"));
    };
    if cli_flag(value) {
        return Err(format!("{flag} requires a value"));
    }
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
        "  --settings-file <path> Path to local settings (default: VV_AGENT_LOCAL_SETTINGS, V_AGENT_LOCAL_SETTINGS, or local_settings.json)",
        "  --workspace <path>     Workspace directory (default: ./workspace)",
        "  --max-cycles <n>       Max runtime cycles (default: 80)",
        "  --language <locale>    System prompt language (default: zh-CN)",
        "  --agent-type <type>    Agent type, e.g. computer",
        "  --temperature <n>      Model sampling temperature",
        "  --top-p <n>            Model nucleus sampling threshold",
        "  --max-tokens <n>       Maximum generated tokens",
        "  --verbose             Show per-cycle runtime logs",
        "",
        "App Server:",
        "  app-server --listen stdio --settings <path> --backend <key> --model <key>",
        "    [--timeout-seconds <seconds>]",
        "  app-server generate-json-schema --out <dir>",
        "  app-server schema --out <dir>",
        "  app-server generate-ts --out <dir>",
    ]
    .join("\n")
}
