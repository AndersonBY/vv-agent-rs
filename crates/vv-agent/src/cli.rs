use std::env;

use crate::config::build_vv_llm_from_local_settings;
use crate::runtime::AgentRuntime;
use crate::sdk::{AgentDefinition, AgentSDKClient, AgentSDKOptions};

pub fn main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let prompt = args
        .find(|arg| arg == "--prompt")
        .and_then(|_| args.next())
        .unwrap_or_else(|| "Hello from vv-agent-rs".to_string());

    let settings_file =
        env::var("V_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.py".to_string());
    let (llm, _) = build_vv_llm_from_local_settings(settings_file, "moonshot", "kimi-k2.5", 90.0)
        .map_err(|err| err.to_string())?;
    let runtime = AgentRuntime::new(llm);
    let client = AgentSDKClient::new(AgentSDKOptions::default()).with_runtime(runtime);
    let agent = AgentDefinition::default_for_model("demo");
    let run = client
        .run_with_agent(agent, prompt)
        .map_err(|err| err.to_string())?;
    println!("{}", run.result.final_answer.unwrap_or_default());
    Ok(())
}
