mod common;

use common::{env_usize, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentRun, AgentSDKClient, AgentSDKOptions, AgentStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let max_retries = env_usize("V_AGENT_EXAMPLE_MAX_RETRIES", 2);

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = "你是可靠执行 Agent. 完成任务后必须调用 `task_finish`.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 8;
    agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = run_with_recovery(
        &client,
        "请列出 workspace 下的文件并输出简要说明, 然后调用 `task_finish`。",
        max_retries,
    )?;
    print_run(&run)
}

fn run_with_recovery(
    client: &AgentSDKClient,
    prompt: &str,
    max_retries: usize,
) -> Result<AgentRun, String> {
    let mut last_error = None::<String>;
    let mut last_run = None::<AgentRun>;
    for attempt in 1..=max_retries + 1 {
        let effective_prompt = if let Some(error) = &last_error {
            format!(
                "[重试 #{}] 上次执行失败: {error}\n请调整策略后重新执行:\n{prompt}",
                attempt - 1
            )
        } else {
            prompt.to_string()
        };
        eprintln!("\n--- Attempt {attempt}/{} ---", max_retries + 1);
        let run = client.run(effective_prompt)?;
        match run.result.status {
            AgentStatus::Completed | AgentStatus::WaitUser => return Ok(run),
            AgentStatus::MaxCycles => {
                last_error = Some(format!("Reached max cycles ({})", run.result.cycles.len()));
            }
            AgentStatus::Failed => {
                last_error = Some(
                    run.result
                        .error
                        .clone()
                        .unwrap_or_else(|| "Unknown failure".to_string()),
                );
            }
            _ => return Ok(run),
        }
        last_run = Some(run);
    }
    last_run.ok_or_else(|| "No run was attempted".to_string())
}
