mod args;
mod logging;
mod output;
mod task;

use std::env;
use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use crate::app_server::processor::MessageProcessor;
use crate::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle, AppClientInfo,
    JsonRpcMessage, ThreadStartParams, TurnStartParams, UserInput,
};
use crate::app_server::test_support::{finish_response, scripted_app_server_client};
use crate::app_server::transport::stdio::{parse_jsonl_message, serialize_jsonl_message};
use crate::app_server::transport::ConnectionId;
use crate::config::build_vv_llm_from_local_settings;
use crate::runtime::AgentRuntime;
use crate::workspace::LocalWorkspaceBackend;

pub use self::args::{
    parse_cli_args_from, parse_cli_args_from_with_default_settings, parse_cli_command_from,
    parse_cli_command_from_with_default_settings, AppServerCliCommand, CliArgs, CliCommand,
    DebugCliCommand,
};
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
    let command = parse_cli_command_from(raw_args)?;
    match command {
        CliCommand::Run(args) => run_task(args),
        CliCommand::AppServer(command) => run_app_server_command(command),
        CliCommand::Debug(command) => run_debug_command(command),
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
    runtime.log_handler = build_cli_log_handler(args.verbose);

    let task = build_cli_task_from_resolved(&args, &resolved, generate_task_id())?;
    let result = runtime.run(task).map_err(|err| err.to_string())?;
    let payload = result_payload(&result, &resolved);
    let output = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    println!("{output}");
    Ok(())
}

fn run_app_server_command(command: AppServerCliCommand) -> Result<(), String> {
    match command {
        AppServerCliCommand::ListenStdio => run_app_server_stdio(),
        AppServerCliCommand::GenerateTs { out } => write_schema_bundle(
            &out,
            generate_app_server_typescript_bundle().map_err(|error| error.to_string())?,
            None,
        ),
        AppServerCliCommand::GenerateJsonSchema { out } => write_schema_bundle(
            &out,
            generate_app_server_json_schema_bundle().map_err(|error| error.to_string())?,
            Some("json"),
        ),
    }
}

fn run_debug_command(command: DebugCliCommand) -> Result<(), String> {
    match command {
        DebugCliCommand::AppServerSendMessage { message } => run_debug_app_server_message(message),
    }
}

fn run_app_server_stdio() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let (mut processor, mut outgoing) = MessageProcessor::new_for_tests(128);
        let connection_id = ConnectionId::new(1);
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        for line in stdin.lock().lines() {
            let line = line.map_err(|error| error.to_string())?;
            let Some(message) =
                parse_jsonl_message(&line).map_err(|error| error.message().to_string())?
            else {
                continue;
            };
            processor.process_message(connection_id, message).await;
            while let Ok(envelope) = outgoing.try_recv() {
                let line = serialize_jsonl_message(&envelope.message)
                    .map_err(|error| error.message().to_string())?;
                stdout
                    .write_all(line.as_bytes())
                    .map_err(|error| error.to_string())?;
                stdout.flush().map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    })
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
                cwd: None,
                title: Some("debug".to_string()),
                model: Some("demo-model".to_string()),
                ephemeral: true,
            })
            .await
            .map_err(|error| error.to_string())?
            .thread;
        client
            .start_turn(TurnStartParams {
                thread_id: thread.id,
                input: vec![UserInput {
                    text: message.clone(),
                }],
                model: Some("demo-model".to_string()),
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
