mod args;
mod logging;
mod output;
mod task;

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use crate::app_server::host::DefaultAppServerHost;
use crate::app_server::processor::MessageProcessor;
use crate::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle, AppClientInfo,
    AppModelInfo, JsonRpcMessage, ThreadStartParams, TurnStartParams,
};
use crate::app_server::server::AppServer;
use crate::app_server::test_support::{finish_response, scripted_app_server_client};
use crate::app_server::thread_store::SqliteThreadStore;
use crate::app_server::transport::stdio::StdioJsonlTransport;
use crate::config::{build_vv_llm_from_local_settings, ResolvedModelConfig};
use crate::llm::LlmClient;
use crate::model::{ModelError, ModelProvider};
use crate::runtime::AgentRuntime;
use crate::workspace::LocalWorkspaceBackend;
use crate::{Agent, ModelRef, RunConfig, Runner};

pub use self::args::{
    parse_cli_args_from, parse_cli_args_from_with_default_settings, parse_cli_command_from,
    parse_cli_command_from_with_default_settings, AppServerCliCommand, CliArgs, CliCommand,
    DebugCliCommand,
};
use self::logging::build_cli_event_handler;
pub use self::output::result_payload;
use self::task::generate_task_id;
pub use self::task::{build_cli_task, build_cli_task_from_resolved};

const APP_SERVER_DEFAULT_WORKSPACE: &str = "./workspace";
const APP_SERVER_DEFAULT_MAX_CYCLES: u32 = 80;
const APP_SERVER_APPROVAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
enum CliError {
    Usage(String),
    Configuration(String),
    Runtime(String),
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Configuration(_) => 2,
            Self::Runtime(_) => 1,
        }
    }

    fn runtime(error: impl ToString) -> Self {
        Self::Runtime(error.to_string())
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::Configuration(message) | Self::Runtime(message) => {
                formatter.write_str(message)
            }
        }
    }
}

#[derive(Clone)]
struct FixedAppServerModelProvider {
    client: Arc<dyn LlmClient>,
    resolved: ResolvedModelConfig,
}

impl FixedAppServerModelProvider {
    fn new(client: Arc<dyn LlmClient>, resolved: ResolvedModelConfig) -> Self {
        Self { client, resolved }
    }

    fn matches(&self, model: &ModelRef) -> bool {
        match model {
            ModelRef::Named(model) => self.matches_model_name(model),
            ModelRef::BackendModel { backend, model } => {
                backend == &self.resolved.backend && self.matches_model_name(model)
            }
            ModelRef::Resolved(resolved) => {
                resolved.backend == self.resolved.backend
                    && resolved.model_id == self.resolved.model_id
            }
        }
    }

    fn matches_model_name(&self, model: &str) -> bool {
        model == self.resolved.requested_model
            || model == self.resolved.selected_model
            || model == self.resolved.model_id
    }
}

impl ModelProvider for FixedAppServerModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        if !self.matches(model) {
            return Err(ModelError::Config(format!(
                "App Server fixed model provider cannot resolve `{}`",
                model.model()
            )));
        }
        Ok(self.resolved.clone())
    }

    fn client(&self, resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        if resolved.backend != self.resolved.backend || resolved.model_id != self.resolved.model_id
        {
            return Err(ModelError::Config(format!(
                "App Server fixed model provider has no client for `{}`",
                resolved.model_id
            )));
        }
        Ok(self.client.clone())
    }

    fn default_model_ref(&self) -> Option<ModelRef> {
        Some(ModelRef::resolved(self.resolved.clone()))
    }
}

fn run_main() -> Result<(), CliError> {
    let raw_args = env::args().collect::<Vec<_>>();
    if raw_args
        .iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("{}", args::help_text());
        return Ok(());
    }
    let app_server_invocation = raw_args.get(1).map(String::as_str) == Some("app-server");
    let command = parse_cli_command_from(raw_args).map_err(|error| {
        if app_server_invocation {
            CliError::Usage(error)
        } else {
            CliError::Runtime(error)
        }
    })?;
    match command {
        CliCommand::Run(args) => run_task(args).map_err(CliError::runtime),
        CliCommand::AppServer(command) => run_app_server_command(command),
        CliCommand::Debug(command) => run_debug_command(command).map_err(CliError::runtime),
    }
}

pub fn main() -> Result<(), String> {
    run_main().map_err(|error| error.to_string())
}

pub fn process_main() -> ExitCode {
    match run_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn run_task(args: CliArgs) -> Result<(), String> {
    let (llm, resolved) =
        build_vv_llm_from_local_settings(&args.settings_file, &args.backend, &args.model, 90.0)
            .map_err(|err| err.to_string())?;

    let mut runtime = AgentRuntime::new(llm)
        .with_settings_file(args.settings_file.clone())
        .with_default_backend(args.backend.clone());
    runtime.default_workspace = Some(args.workspace.clone());
    runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(args.workspace.clone()));
    runtime.event_handler = build_cli_event_handler(args.verbose);

    let task = build_cli_task_from_resolved(&args, &resolved, generate_task_id())?;
    let result = runtime.run(task).map_err(|err| err.to_string())?;
    let payload = result_payload(&result, &resolved);
    let output = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    println!("{output}");
    Ok(())
}

fn run_app_server_command(command: AppServerCliCommand) -> Result<(), CliError> {
    match command {
        AppServerCliCommand::ListenStdio {
            settings_file,
            backend,
            model,
            timeout_seconds,
        } => run_app_server_stdio(settings_file, backend, model, timeout_seconds),
        AppServerCliCommand::GenerateTs { out } => write_schema_bundle(
            &out,
            generate_app_server_typescript_bundle().map_err(CliError::runtime)?,
            None,
        )
        .map_err(CliError::runtime),
        AppServerCliCommand::GenerateJsonSchema { out } => write_json_schema_bundle(
            &out,
            generate_app_server_json_schema_bundle().map_err(CliError::runtime)?,
        )
        .map_err(CliError::runtime),
    }
}

fn run_debug_command(command: DebugCliCommand) -> Result<(), String> {
    match command {
        DebugCliCommand::AppServerSendMessage { message } => run_debug_app_server_message(message),
    }
}

fn run_app_server_stdio(
    settings_file: PathBuf,
    backend: String,
    model: String,
    timeout_seconds: f64,
) -> Result<(), CliError> {
    let workspace = PathBuf::from(APP_SERVER_DEFAULT_WORKSPACE);
    let (processor, outgoing) = production_app_server_processor(
        &settings_file,
        &backend,
        &model,
        &workspace,
        APP_SERVER_DEFAULT_MAX_CYCLES,
        timeout_seconds,
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(CliError::runtime)?;
    runtime
        .block_on(async {
            AppServer::new(StdioJsonlTransport::new(), processor, outgoing)
                .run()
                .await
                .map_err(|error| error.message().to_string())
        })
        .map_err(CliError::runtime)
}

fn production_app_server_processor(
    settings_file: &Path,
    backend: &str,
    model: &str,
    workspace: &Path,
    max_cycles: u32,
    timeout_seconds: f64,
) -> Result<
    (
        MessageProcessor,
        tokio::sync::mpsc::Receiver<crate::app_server::outgoing::OutgoingEnvelope>,
    ),
    CliError,
> {
    let (llm, resolved) =
        build_vv_llm_from_local_settings(settings_file, backend, model, timeout_seconds)
            .map_err(|error| CliError::Configuration(error.to_string()))?;
    let model_provider: Arc<dyn ModelProvider> = Arc::new(FixedAppServerModelProvider::new(
        Arc::new(llm),
        resolved.clone(),
    ));
    let run_config = RunConfig {
        model: Some(ModelRef::resolved(resolved.clone())),
        model_provider: Some(model_provider.clone()),
        workspace: Some(workspace.to_path_buf()),
        workspace_backend: Some(Arc::new(LocalWorkspaceBackend::new(
            workspace.to_path_buf(),
        ))),
        max_cycles: Some(max_cycles),
        approval_timeout: Some(APP_SERVER_APPROVAL_TIMEOUT),
        ..RunConfig::default()
    };
    let runner = Runner::builder()
        .model_provider_arc(model_provider)
        .workspace(workspace)
        .default_run_config(run_config.clone())
        .build()
        .map_err(CliError::runtime)?;
    let agent = Agent::builder("assistant")
        .instructions(
            "You are the vv-agent App Server assistant. Complete user requests with available tools.",
        )
        .model(ModelRef::resolved(resolved.clone()))
        .build()
        .map_err(CliError::runtime)?;
    let mut metadata = std::collections::BTreeMap::new();
    metadata.insert(
        "requestedModel".to_string(),
        serde_json::json!(resolved.requested_model),
    );
    let host = DefaultAppServerHost::new()
        .with_agent(agent)
        .with_run_config(run_config)
        .with_models(vec![AppModelInfo {
            id: resolved.model_id,
            provider: Some(resolved.backend),
            display_name: Some(resolved.selected_model),
            context_length: resolved.context_length,
            supports_tools: resolved.function_call_available,
            metadata,
        }]);
    let store = SqliteThreadStore::in_memory().map_err(CliError::runtime)?;
    Ok(MessageProcessor::with_host(
        128,
        runner,
        Arc::new(host),
        store,
    ))
}

fn run_debug_app_server_message(message: String) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let mut client = scripted_app_server_client(vec![finish_response(&message)]);
        client
            .initialize(AppClientInfo {
                name: "debug-cli".to_string(),
                title: Some("Debug CLI".to_string()),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            })
            .await
            .map_err(|error| error.to_string())?;
        let thread = client
            .start_thread(ThreadStartParams {
                agent_key: "default".to_string(),
                cwd: None,
                metadata: Default::default(),
            })
            .await
            .map_err(|error| error.to_string())?
            .thread_id;
        client
            .start_turn(TurnStartParams {
                thread_id: thread,
                input: vec![serde_json::json!({"type": "text", "text": message.clone()})],
                metadata: Default::default(),
            })
            .await
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "method": "debug/input",
                "params": { "message": message }
            }))
            .map_err(|error| error.to_string())?
        );
        while let Some(next) = client.next_message().await {
            println!(
                "{}",
                serde_json::to_string(&next).map_err(|error| error.to_string())?
            );
            if matches!(
                next,
                JsonRpcMessage::Notification(notification)
                    if notification.method == "turn/completed"
            ) {
                break;
            }
        }
        Ok(())
    })
}

fn write_schema_bundle(
    out: &Path,
    bundle: crate::app_server::protocol::SchemaBundle,
    extension: Option<&str>,
) -> Result<(), String> {
    fs::create_dir_all(out).map_err(|error| error.to_string())?;
    for (name, content) in bundle {
        let file_name = match extension {
            Some(extension) => format!("{name}.{extension}"),
            None => name,
        };
        fs::write(out.join(file_name), content).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_json_schema_bundle(
    out: &Path,
    bundle: crate::app_server::protocol::SchemaBundle,
) -> Result<(), String> {
    let json_dir = out.join("json");
    fs::create_dir_all(&json_dir).map_err(|error| error.to_string())?;
    let mut aggregate = BTreeMap::new();
    for (name, content) in bundle {
        let schema = serde_json::from_str::<serde_json::Value>(&content)
            .map_err(|error| format!("invalid committed JSON schema {name}: {error}"))?;
        fs::write(json_dir.join(format!("{name}.json")), content)
            .map_err(|error| error.to_string())?;
        aggregate.insert(name, schema);
    }
    let aggregate = serde_json::to_string_pretty(&aggregate).map_err(|error| error.to_string())?;
    fs::write(
        json_dir.join("vv_agent_app_server.schemas.json"),
        format!("{aggregate}\n"),
    )
    .map_err(|error| error.to_string())
}
