mod common;

use common::{
    build_direct_runtime, env_f64, make_task_id, print_agent_result, runtime_log_handler,
    ExampleConfig,
};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::types::AgentTask;
use vv_agent::{CancellationToken, ExecutionContext, RuntimeRunControls, ThreadBackend};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let timeout = env_f64("VV_AGENT_EXAMPLE_TIMEOUT", 10.0).max(0.1);
    let (runtime, resolved) = build_direct_runtime(&config, 90.0)?;
    let runtime = runtime.with_execution_backend(ThreadBackend::new(2));

    let system_prompt = build_system_prompt_with_options(
        "You are a helpful agent. Complete the task step by step.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            ..BuildSystemPromptOptions::default()
        },
    );
    let mut task = AgentTask::new(
        make_task_id("cancel_demo"),
        resolved.model_id,
        system_prompt,
        "写一篇关于人工智能发展历史的长文, 至少 2000 字",
    );
    task.max_cycles = 20;

    let token = CancellationToken::default();
    let timer_token = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs_f64(timeout));
        timer_token.cancel();
    });
    eprintln!("[demo] 任务已启动, {timeout}s 后将自动取消...");

    let controls = RuntimeRunControls {
        workspace: Some(config.workspace),
        event_handler: runtime_log_handler(config.verbose),
        execution_context: Some(ExecutionContext::default().with_cancellation_token(token)),
        ..RuntimeRunControls::default()
    };
    let result = runtime.run_with_controls(task, controls)?;
    print_agent_result(&result)
}
