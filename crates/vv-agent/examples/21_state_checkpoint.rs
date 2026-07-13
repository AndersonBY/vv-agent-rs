mod common;

use common::{
    build_direct_runtime, env_string, make_task_id, print_agent_result, runtime_log_handler,
    ExampleConfig,
};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{
    AgentTask, Checkpoint, ExecutionContext, InMemoryStateStore, RuntimeRunControls,
    SqliteStateStore, StateStore,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let db_path = env_string("V_AGENT_EXAMPLE_DB", ":memory:");
    let (runtime, resolved) = build_direct_runtime(&config, 90.0)?;
    let system_prompt = build_system_prompt_with_options(
        "You are a helpful agent.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            ..BuildSystemPromptOptions::default()
        },
    );
    let task_id = make_task_id("ckpt_demo");
    let mut task = AgentTask::new(
        task_id.clone(),
        resolved.model_id,
        system_prompt,
        "2+3 等于几?",
    );
    task.max_cycles = 5;

    let store = std::sync::Arc::new(SqliteStateStore::new(db_path)?);
    eprintln!("[demo] 运行任务 {task_id}...");
    let result = runtime.run_with_controls(
        task,
        RuntimeRunControls {
            workspace: Some(config.workspace),
            log_handler: runtime_log_handler(config.verbose),
            execution_context: Some(ExecutionContext::default().with_state_store(store.clone())),
            ..RuntimeRunControls::default()
        },
    )?;
    print_agent_result(&result)?;

    let checkpoint = Checkpoint {
        task_id: task_id.clone(),
        cycle_index: result.cycles.len() as u32,
        status: result.status,
        messages: result.messages.clone(),
        cycles: result.cycles.clone(),
        shared_state: result.shared_state.clone(),
        revision: 0,
        claim_token: None,
        claimed_cycle: None,
        lease_expires_at_ms: None,
        terminal_result: None,
    };
    store.save_checkpoint(checkpoint.clone())?;
    println!("[demo] Checkpoint 已保存, task_id={task_id}");
    println!(
        "[demo] 当前 checkpoint 列表: {:?}",
        store.list_checkpoints()?
    );
    if let Some(loaded) = store.load_checkpoint(&task_id)? {
        println!(
            "[demo] 加载成功: cycle_index={}, status={:?}, messages={}, cycles={}",
            loaded.cycle_index,
            loaded.status,
            loaded.messages.len(),
            loaded.cycles.len()
        );
    }
    store.delete_checkpoint(&task_id)?;
    println!(
        "[demo] Checkpoint 已删除, 剩余: {:?}",
        store.list_checkpoints()?
    );

    let mem_store = InMemoryStateStore::new();
    mem_store.save_checkpoint(checkpoint)?;
    println!(
        "[demo] InMemory save -> list: {:?}",
        mem_store.list_checkpoints()?
    );
    mem_store.delete_checkpoint(&task_id)?;
    println!(
        "[demo] InMemory delete -> list: {:?}",
        mem_store.list_checkpoints()?
    );
    Ok(())
}
