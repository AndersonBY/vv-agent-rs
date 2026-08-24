//! Framework-owned deferred-tool identity construction.
//!
//! A real tool handler calls `ToolContext::defer()` before handing the opaque
//! handle to its external provider. The provider payload and callback policy
//! stay outside vv-agent; this example only demonstrates the closed outcome
//! and the fail-closed non-durable path.

use vv_agent::{ToolCallOutcome, ToolContext};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut context = ToolContext::new(".");
    let non_durable = context.defer();
    assert!(matches!(
        non_durable,
        ToolCallOutcome::Completed { ref result }
            if result.error_code.as_deref() == Some("deferred_requires_checkpoint")
    ));

    context.set_deferred_identity(
        "example/checkpoint",
        "op_tool_cycle_1_call_remote",
        1,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );
    let durable = context.defer();
    let ToolCallOutcome::Deferred { handle } = durable else {
        return Err("checkpoint identity should produce a deferred handle".into());
    };
    handle.validate()?;
    println!("deferred handle: {handle}");
    println!(
        "outcome: {}",
        serde_json::to_string(&ToolCallOutcome::deferred(handle))?
    );
    Ok(())
}
