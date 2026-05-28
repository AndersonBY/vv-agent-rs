mod common;

use common::{
    build_direct_runtime, make_task_id, print_agent_result, runtime_log_handler, ExampleConfig,
};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{AgentTask, CeleryBackend, RuntimeRunControls};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let (runtime, resolved) = build_direct_runtime(&config, 90.0)?;
    let celery_backend = CeleryBackend::inline_fallback();
    let runtime = runtime.with_execution_backend(celery_backend.clone());

    let system_prompt = build_system_prompt_with_options(
        "You are a helpful agent.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            ..BuildSystemPromptOptions::default()
        },
    );
    eprintln!("[demo] 场景 1: CeleryBackend eager/inline fallback 执行");
    let mut task = AgentTask::new(
        make_task_id("celery_inline"),
        resolved.model_id,
        system_prompt,
        "1+1 等于几?",
    );
    task.max_cycles = 3;
    let result = runtime.run_with_controls(
        task,
        RuntimeRunControls {
            workspace: Some(config.workspace),
            log_handler: runtime_log_handler(config.verbose),
            ..RuntimeRunControls::default()
        },
    )?;
    print_agent_result(&result)?;

    eprintln!("[demo] 场景 2: parallel_map 通过 CeleryBackend 接口并行/回退执行");
    let prompts = vec![
        "Rust 的所有权模型是什么? 一句话回答".to_string(),
        "什么是 REST API? 一句话回答".to_string(),
        "Docker 和虚拟机的区别? 一句话回答".to_string(),
    ];
    let answers =
        celery_backend.parallel_map(|prompt| format!("queued prompt: {prompt}"), prompts.clone());
    for (prompt, answer) in prompts.iter().zip(answers.iter()) {
        println!("  Q: {prompt}\n  A: {answer}\n");
    }
    Ok(())
}
