mod common;

use common::{
    build_direct_runtime, make_task_id, print_agent_result, runtime_log_handler, ExampleConfig,
};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::types::AgentTask;
use vv_agent::{CancellationToken, ExecutionContext, RuntimeRunControls, ThreadBackend};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let (runtime, resolved) = build_direct_runtime(&config, 90.0)?;
    let thread_backend = ThreadBackend::new(4);
    let runtime = runtime.with_execution_backend(thread_backend.clone());
    let system_prompt = build_system_prompt_with_options(
        "You are a helpful agent.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            ..BuildSystemPromptOptions::default()
        },
    );

    eprintln!("[demo] 方式 1: 同步调用 runtime.run()");
    let mut task1 = AgentTask::new(
        make_task_id("thread_sync"),
        resolved.model_id.clone(),
        system_prompt.clone(),
        "1+1 等于几?",
    );
    task1.max_cycles = 3;
    let result1 = runtime.run_with_controls(
        task1,
        RuntimeRunControls {
            workspace: Some(config.workspace.clone()),
            event_handler: runtime_log_handler(config.verbose),
            ..RuntimeRunControls::default()
        },
    )?;
    print_agent_result(&result1)?;

    eprintln!("[demo] 方式 2: 非阻塞 submit, 主线程可做其他事");
    let mut task2 = AgentTask::new(
        make_task_id("thread_async"),
        resolved.model_id,
        system_prompt,
        "Rust 的所有权模型是什么? 一句话回答",
    );
    task2.max_cycles = 3;
    let token = CancellationToken::default();
    let controls = RuntimeRunControls {
        workspace: Some(config.workspace),
        event_handler: runtime_log_handler(config.verbose),
        execution_context: Some(ExecutionContext::default().with_cancellation_token(token)),
        ..RuntimeRunControls::default()
    };
    let handle = thread_backend.submit(move || runtime.run_with_controls(task2, controls));
    eprintln!("  [主线程] Future 已提交, 正在做其他事...");
    std::thread::sleep(std::time::Duration::from_millis(500));
    eprintln!("  [主线程] 等待结果...");
    let result2 = handle.join().expect("thread backend worker panicked")?;
    print_agent_result(&result2)
}
