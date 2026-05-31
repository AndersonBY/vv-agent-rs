mod common;

use common::{build_facade_agent, build_facade_runner, print_run_result, ExampleConfig};
use vv_agent::{MemorySession, RunConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let runner = build_facade_runner(&config)?;
    let agent = build_facade_agent(
        &config,
        "session-demo",
        "你是长会话助手。回答时利用前文，但只输出当前轮最终答案。",
    )?;
    let session = MemorySession::new("example-session");
    let first_prompt = config
        .prompt
        .clone()
        .unwrap_or_else(|| "记住：这个项目叫 vv-agent-rs。".to_string());
    let _first = runner
        .run_with_config(
            &agent,
            first_prompt,
            RunConfig::builder()
                .session(session.clone())
                .max_cycles(6)
                .build(),
        )
        .await?;
    let second = runner
        .run_with_config(
            &agent,
            "刚才提到的项目名是什么？",
            RunConfig::builder().session(session).max_cycles(6).build(),
        )
        .await?;
    print_run_result(&second)
}
