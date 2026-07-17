mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{
    build_facade_agent, build_facade_runner, env_string, print_run_result, ExampleConfig,
};
use vv_agent::{
    CheckpointConfig, CheckpointStoreV2, ResumePolicy, RunConfig, SqliteCheckpointStoreV2,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let db_path = PathBuf::from(env_string(
        "V_AGENT_EXAMPLE_DB",
        &config
            .workspace
            .join(".vv-agent-state/checkpoints-v2.db")
            .to_string_lossy(),
    ));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let checkpoint_key = env_string(
        "V_AGENT_EXAMPLE_CHECKPOINT_KEY",
        "example-21-state-checkpoint",
    );
    let prompt = config
        .prompt
        .clone()
        .unwrap_or_else(|| "Calculate 2+3, briefly verify the result, and finish.".to_string());
    let store = Arc::new(SqliteCheckpointStoreV2::new(&db_path)?);
    let mut checkpoint = CheckpointConfig::new(store.clone());
    checkpoint.key = Some(checkpoint_key.clone());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;

    let runner = build_facade_runner(&config)?;
    let agent = build_facade_agent(
        &config,
        "checkpoint-demo",
        "Complete the requested task carefully. Finish only after the answer is ready.",
    )?;

    println!("[demo] checkpoint={checkpoint_key}");
    println!("[demo] database={}", db_path.display());
    let result = runner
        .run_with_config(
            &agent,
            prompt,
            RunConfig::builder()
                .max_cycles(5)
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await?;
    print_run_result(&result)?;

    if let Some(retained) = store.load_checkpoint_v2(&checkpoint_key)? {
        println!(
            "[demo] durable_state=cycle:{} resume_attempt:{} terminal_acknowledged:{}",
            retained.cycle_index, retained.resume_attempt, retained.terminal_acknowledged
        );
    }
    println!("[demo] Run the same command again to replay or resume this checkpoint.");
    Ok(())
}
