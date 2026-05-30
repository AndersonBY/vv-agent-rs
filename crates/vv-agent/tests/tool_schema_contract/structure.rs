use std::path::Path;

use super::helpers::collect_rust_files;
use super::MAX_REASONABLE_SOURCE_LINES;

#[test]
fn tools_module_is_split_into_handler_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(root.join("tools").join("mod.rs").is_file());
    for relative in [
        "tools/base.rs",
        "tools/common.rs",
        "tools/common/args.rs",
        "tools/common/edit.rs",
        "tools/common/file_types.rs",
        "tools/common/grep.rs",
        "tools/common/paths.rs",
        "tools/common/process.rs",
        "tools/common/result.rs",
        "tools/dispatcher.rs",
        "tools/registry/mod.rs",
        "tools/registry/defaults.rs",
        "tools/schemas/mod.rs",
        "tools/schemas/command.rs",
        "tools/schemas/control.rs",
        "tools/schemas/media.rs",
        "tools/schemas/memory.rs",
        "tools/schemas/sub_agents/mod.rs",
        "tools/schemas/sub_agents/create.rs",
        "tools/schemas/sub_agents/status.rs",
        "tools/schemas/todo.rs",
        "tools/schemas/workspace/mod.rs",
        "tools/schemas/workspace/edit.rs",
        "tools/schemas/workspace/file_io.rs",
        "tools/schemas/workspace/listing.rs",
        "tools/schemas/workspace/search.rs",
        "tools/handlers/control.rs",
        "tools/handlers/todo.rs",
        "tools/handlers/workspace/mod.rs",
        "tools/handlers/workspace/edit.rs",
        "tools/handlers/workspace/file_io.rs",
        "tools/handlers/workspace/file_io/info.rs",
        "tools/handlers/workspace/file_io/read.rs",
        "tools/handlers/workspace/file_io/write.rs",
        "tools/handlers/workspace/listing.rs",
        "tools/handlers/workspace/listing/fallback.rs",
        "tools/handlers/workspace/listing/local_rg.rs",
        "tools/handlers/workspace/listing/local_rg/command.rs",
        "tools/handlers/workspace/listing/local_rg/paths.rs",
        "tools/handlers/workspace/listing/local_rg/scan.rs",
        "tools/handlers/workspace/listing/local_rg/tests.rs",
        "tools/handlers/workspace/listing/local_rg/types.rs",
        "tools/handlers/workspace/listing/request.rs",
        "tools/handlers/workspace/listing/response.rs",
        "tools/handlers/workspace/listing/types.rs",
        "tools/handlers/search/mod.rs",
        "tools/handlers/search/error.rs",
        "tools/handlers/search/execution.rs",
        "tools/handlers/search/fallback.rs",
        "tools/handlers/search/format.rs",
        "tools/handlers/search/local_rg.rs",
        "tools/handlers/search/local_rg/command.rs",
        "tools/handlers/search/local_rg/parse.rs",
        "tools/handlers/search/local_rg/parse/decode.rs",
        "tools/handlers/search/local_rg/parse/events.rs",
        "tools/handlers/search/local_rg/parse/paths.rs",
        "tools/handlers/search/local_rg/parse/state.rs",
        "tools/handlers/search/local_rg/paths.rs",
        "tools/handlers/search/response.rs",
        "tools/handlers/search/request.rs",
        "tools/handlers/search/local_rg/tests.rs",
        "tools/handlers/search/local_rg/types.rs",
        "tools/handlers/bash.rs",
        "tools/handlers/bash/env.rs",
        "tools/handlers/bash/execution.rs",
        "tools/handlers/bash/shell_defaults.rs",
        "tools/handlers/image.rs",
        "tools/handlers/memory.rs",
        "tools/handlers/skills/mod.rs",
        "tools/handlers/skills/state.rs",
        "tools/handlers/sub_agents.rs",
        "tools/handlers/sub_agents/async_mode.rs",
        "tools/handlers/sub_agents/batch.rs",
        "tools/handlers/sub_agents/request.rs",
        "tools/handlers/sub_agents/response.rs",
        "tools/handlers/sub_task_status.rs",
        "tools/handlers/background.rs",
        "runtime/mod.rs",
        "runtime/backends/mod.rs",
        "runtime/backends/inline.rs",
        "runtime/backends/recipe.rs",
        "runtime/backends/results.rs",
        "runtime/backends/thread.rs",
        "runtime/background_sessions.rs",
        "runtime/background_sessions/listeners.rs",
        "runtime/background_sessions/options.rs",
        "runtime/background_sessions/session.rs",
        "runtime/background_sessions/subscription.rs",
        "runtime/background_sessions/tests.rs",
        "runtime/backends/celery.rs",
        "runtime/backends/celery/backend.rs",
        "runtime/backends/celery/checkpoint.rs",
        "runtime/backends/celery/dispatch.rs",
        "runtime/backends/celery/distributed.rs",
        "runtime/backends/celery/execution.rs",
        "runtime/backends/celery_tasks.rs",
        "runtime/cancellation.rs",
        "runtime/cycle_runner.rs",
        "runtime/engine/completion.rs",
        "runtime/engine/construction.rs",
        "runtime/engine/mod.rs",
        "runtime/engine/controls.rs",
        "runtime/engine/cycle_inputs.rs",
        "runtime/engine/helpers.rs",
        "runtime/engine/logging.rs",
        "runtime/engine/memory.rs",
        "runtime/engine/memory/callbacks.rs",
        "runtime/engine/memory/metadata.rs",
        "runtime/engine/planning.rs",
        "runtime/engine/memory/session.rs",
        "runtime/engine/memory/token_limits.rs",
        "runtime/engine/run_setup.rs",
        "runtime/engine/state.rs",
        "runtime/hooks.rs",
        "runtime/hooks/events.rs",
        "runtime/hooks/manager.rs",
        "runtime/hooks/patches.rs",
        "runtime/hooks/traits.rs",
        "runtime/processes.rs",
        "runtime/processes/capture.rs",
        "runtime/processes/output.rs",
        "runtime/processes/platform.rs",
        "runtime/processes/termination.rs",
        "runtime/results.rs",
        "runtime/shell/mod.rs",
        "runtime/shell/command.rs",
        "runtime/shell/metadata.rs",
        "runtime/shell/path.rs",
        "runtime/shell/platform.rs",
        "runtime/shell/windows.rs",
        "runtime/shell/windows/discovery.rs",
        "runtime/shell/windows/priority.rs",
        "runtime/shell/windows/programs.rs",
        "runtime/shell/windows/resolve.rs",
        "runtime/shell/windows/tests.rs",
        "runtime/sub_agents/mod.rs",
        "runtime/sub_agents/events.rs",
        "runtime/sub_agents/runner.rs",
        "runtime/sub_agents/runner/identity.rs",
        "runtime/sub_agents/runner/model.rs",
        "runtime/sub_agents/runner/outcome.rs",
        "runtime/sub_agents/runner/session.rs",
        "runtime/sub_agents/session.rs",
        "runtime/sub_agents/session/events.rs",
        "runtime/sub_agents/session/execution.rs",
        "runtime/sub_agents/session/state.rs",
        "runtime/sub_agents/session/subscription.rs",
        "runtime/sub_agents/task.rs",
        "runtime/sub_agents/types.rs",
        "runtime/sub_task_manager/mod.rs",
        "runtime/sub_task_manager/events.rs",
        "runtime/sub_task_manager/helpers.rs",
        "runtime/sub_task_manager/identity.rs",
        "runtime/sub_task_manager/manager.rs",
        "runtime/sub_task_manager/record.rs",
        "runtime/sub_task_manager/sessions.rs",
        "runtime/sub_task_manager/status.rs",
        "runtime/sub_task_manager/submission.rs",
        "runtime/sub_task_manager/types.rs",
        "runtime/token_usage.rs",
        "runtime/tool_call_runner.rs",
        "runtime/tool_call_runner/outcome.rs",
        "runtime/tool_call_runner/request.rs",
        "runtime/tool_call_runner/results.rs",
        "runtime/tool_call_runner/runner.rs",
        "runtime/tool_planner.rs",
        "skills/mod.rs",
        "skills/errors.rs",
        "skills/models.rs",
        "skills/normalize.rs",
        "skills/normalize/path.rs",
        "skills/normalize/value.rs",
        "skills/parser.rs",
        "skills/parser/discovery.rs",
        "skills/parser/frontmatter.rs",
        "skills/parser/io.rs",
        "skills/parser/properties.rs",
        "skills/parser/read.rs",
        "skills/parser/value.rs",
        "skills/prompt.rs",
        "skills/validator.rs",
        "skills/validator/diagnostics.rs",
        "skills/validator/mode.rs",
        "skills/validator/rules.rs",
        "memory/artifacts.rs",
        "memory/artifacts/config.rs",
        "memory/artifacts/content.rs",
        "memory/artifacts/info.rs",
        "memory/artifacts/persist.rs",
        "memory/artifacts/render.rs",
        "memory/microcompact.rs",
        "memory/mod.rs",
        "memory/manager/mod.rs",
        "memory/manager/compaction.rs",
        "memory/manager/config.rs",
        "memory/manager/emergency.rs",
        "memory/manager/helpers.rs",
        "memory/manager/limits.rs",
        "memory/manager/microcompact.rs",
        "memory/manager/normalization.rs",
        "memory/manager/prompts.rs",
        "memory/manager/session_context.rs",
        "memory/manager/warnings.rs",
        "memory/session/mod.rs",
        "memory/session/config.rs",
        "memory/session/entry.rs",
        "memory/session/parse.rs",
        "memory/session/prompt.rs",
        "memory/session/state.rs",
        "memory/session/storage.rs",
        "memory/summary.rs",
        "memory/summary/events.rs",
        "memory/summary/files.rs",
        "memory/summary/original.rs",
        "memory/summary/text.rs",
        "memory/token_utils.rs",
        "prompt/mod.rs",
        "prompt/builder.rs",
        "prompt/cache_tracker.rs",
        "prompt/templates.rs",
        "llm/mod.rs",
        "llm/base.rs",
        "llm/scripted.rs",
        "llm/anthropic_prompt_cache.rs",
        "llm/anthropic_prompt_cache/blocks.rs",
        "llm/anthropic_prompt_cache/breakpoints.rs",
        "llm/anthropic_prompt_cache/estimate.rs",
        "llm/anthropic_prompt_cache/model.rs",
        "llm/anthropic_prompt_cache/sections.rs",
        "llm/vv_llm_client/mod.rs",
        "llm/vv_llm_client/construction.rs",
        "llm/vv_llm_client/endpoints.rs",
        "llm/vv_llm_client/execution.rs",
        "llm/vv_llm_client/model_rules.rs",
        "llm/vv_llm_client/prompt_cache.rs",
        "llm/vv_llm_client/prompt_cache/apply.rs",
        "llm/vv_llm_client/prompt_cache/endpoint.rs",
        "llm/vv_llm_client/prompt_cache/from_cache.rs",
        "llm/vv_llm_client/prompt_cache/metadata.rs",
        "llm/vv_llm_client/prompt_cache/to_cache.rs",
        "llm/vv_llm_client/request.rs",
        "llm/vv_llm_client/response.rs",
        "llm/vv_llm_client/streaming.rs",
        "llm/vv_llm_client/streaming/events.rs",
        "llm/vv_llm_client/streaming/raw_content.rs",
        "llm/vv_llm_client/streaming/tool_calls.rs",
        "workspace/mod.rs",
        "workspace/base.rs",
        "workspace/local.rs",
        "workspace/memory.rs",
        "workspace/s3.rs",
        "workspace/s3/backend.rs",
        "workspace/s3/config.rs",
        "workspace/s3/paths.rs",
        "workspace/s3/runtime.rs",
        "config/settings_literal.rs",
        "config/settings_literal/assignment.rs",
        "config/settings_literal/identifiers.rs",
        "config/settings_literal/json.rs",
        "config/settings_literal/strings.rs",
        "config/model_resolution/aliases.rs",
        "config/model_resolution/backend.rs",
        "config/model_resolution/endpoints.rs",
        "config/model_resolution/settings.rs",
        "constants/mod.rs",
        "constants/tool_names.rs",
        "constants/workspace.rs",
        "types/mod.rs",
        "types/metadata.rs",
        "types/status.rs",
        "types/messages.rs",
        "types/tool_calls.rs",
        "types/token_usage.rs",
        "types/tasks.rs",
        "types/records.rs",
        "types/dict/mod.rs",
        "types/dict/common.rs",
        "types/dict/common/enums.rs",
        "types/dict/common/fields.rs",
        "types/dict/common/values.rs",
        "types/dict/messages.rs",
        "types/dict/records.rs",
        "types/dict/records/cycle.rs",
        "types/dict/records/result.rs",
        "types/dict/records/task.rs",
        "types/dict/token_usage.rs",
        "types/dict/tools.rs",
        "prompt/builder/hash.rs",
        "prompt/builder/options.rs",
        "prompt/builder/section.rs",
        "prompt/builder/system.rs",
        "prompt/builder/system_builder.rs",
        "sdk/mod.rs",
        "sdk/types.rs",
        "sdk/resources.rs",
        "sdk/resources/loader.rs",
        "sdk/resources/models.rs",
        "sdk/resources/parse.rs",
        "sdk/resources/paths.rs",
        "sdk/session/mod.rs",
        "sdk/session/events.rs",
        "sdk/session/handles.rs",
        "sdk/session/run.rs",
        "sdk/session/run/controls.rs",
        "sdk/session/run/execution.rs",
        "sdk/session/run/prompt.rs",
        "sdk/session/run/query.rs",
        "sdk/session/state.rs",
        "sdk/session/util.rs",
        "sdk/session/watchers.rs",
        "sdk/client/mod.rs",
        "sdk/client/agents.rs",
        "sdk/client/queries.rs",
        "sdk/client/runtime.rs",
        "sdk/client/runtime/controls.rs",
        "sdk/client/runtime/llm.rs",
        "sdk/client/runtime/options.rs",
        "sdk/client/runtime/runners.rs",
        "sdk/client/runs.rs",
        "sdk/client/sessions.rs",
        "sdk/client/sessions/base.rs",
        "sdk/client/sessions/defaults.rs",
        "sdk/client/sessions/named.rs",
        "sdk/client/sessions/run.rs",
        "sdk/client/task.rs",
        "sdk/client/task/build.rs",
        "sdk/client/task/defaults.rs",
        "sdk/client/task/ids.rs",
        "sdk/client/task/inline.rs",
        "sdk/client/task/metadata.rs",
        "sdk/client/task/named.rs",
        "sdk/types.rs",
        "sdk/types/definition.rs",
        "sdk/types/options.rs",
        "sdk/types/query.rs",
        "sdk/types/run.rs",
        "cli.rs",
        "cli/args.rs",
        "cli/logging.rs",
        "cli/output.rs",
        "cli/task.rs",
    ] {
        assert!(root.join(relative).is_file(), "missing {relative}");
    }
    for (relative, message) in [
        (
            "tools.rs",
            "tools.rs should be split into src/tools/ modules",
        ),
        (
            "tools/registry.rs",
            "tools registry should be split into src/tools/registry/ modules",
        ),
        (
            "runtime.rs",
            "runtime.rs should be split into src/runtime/ modules",
        ),
        (
            "background_sessions.rs",
            "background sessions should live under src/runtime/",
        ),
        (
            "processes.rs",
            "captured process helpers should live under src/runtime/",
        ),
        (
            "sub_agent_sessions.rs",
            "sub-agent session registry helpers should be exposed through runtime::engine and runtime, not flattened at crate root",
        ),
        (
            "sub_task_manager.rs",
            "sub-task manager should live under src/runtime/sub_task_manager/ modules",
        ),
        (
            "runtime/sub_agents.rs",
            "sub-agent runtime should be split into src/runtime/sub_agents/ modules",
        ),
        (
            "runtime/backends.rs",
            "runtime/backends.rs should be split into src/runtime/backends/ modules",
        ),
        (
            "runtime/engine.rs",
            "runtime/engine.rs should be split into src/runtime/engine/ modules",
        ),
        (
            "runtime/shell.rs",
            "runtime shell helpers should be split into src/runtime/shell/ modules",
        ),
        (
            "memory.rs",
            "memory.rs should be split into src/memory/ modules",
        ),
        (
            "memory/manager.rs",
            "memory manager should be split into src/memory/manager/ modules",
        ),
        (
            "memory/session.rs",
            "session memory should be split into src/memory/session/ modules",
        ),
        (
            "prompt.rs",
            "prompt.rs should be split into src/prompt/ modules",
        ),
        ("llm.rs", "llm.rs should be split into src/llm/ modules"),
        (
            "llm/vv_llm_client.rs",
            "vv-llm client should be split into src/llm/vv_llm_client/ modules",
        ),
        (
            "workspace.rs",
            "workspace.rs should be split into src/workspace/ modules",
        ),
        ("sdk.rs", "sdk.rs should be split into src/sdk/ modules"),
        (
            "sdk/client.rs",
            "SDK client facade should be split into src/sdk/client/ modules",
        ),
        (
            "sdk/session.rs",
            "SDK session runtime should be split into src/sdk/session/ modules",
        ),
        (
            "tools/schemas.rs",
            "schemas.rs should be split into src/tools/schemas/ domain modules",
        ),
        (
            "tools/schemas/sub_agents.rs",
            "sub-agent schemas should be split into create/status modules",
        ),
        (
            "tools/schemas/workspace.rs",
            "workspace schemas should be split into src/tools/schemas/workspace/ modules",
        ),
        (
            "tools/handlers/skills.rs",
            "skills.rs should be split into src/tools/handlers/skills/ modules",
        ),
        (
            "tools/handlers/skills/models.rs",
            "skill models should live in the public src/skills/ module",
        ),
        (
            "tools/handlers/skills/normalize.rs",
            "skill normalization should live in the public src/skills/ module",
        ),
        (
            "tools/handlers/skills/parser.rs",
            "skill parsing should live in the public src/skills/ module",
        ),
        (
            "skills.rs",
            "skills.rs should be split into src/skills/ modules",
        ),
        (
            "constants.rs",
            "constants.rs should be split into src/constants/ modules",
        ),
        (
            "types/dict.rs",
            "dictionary conversions should be split into src/types/dict/ modules",
        ),
        (
            "types.rs",
            "core public types should be split into src/types/ modules",
        ),
    ] {
        assert!(!root.join(relative).exists(), "{message}");
    }
}

#[test]
fn rust_source_files_stay_under_reasonable_size_limit() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_files = collect_rust_files(&manifest_dir.join("src"));
    let oversized = source_files
        .into_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).expect("read source file");
            let line_count = content.lines().count();
            (line_count > MAX_REASONABLE_SOURCE_LINES).then(|| {
                let relative = path
                    .strip_prefix(manifest_dir)
                    .unwrap_or(path.as_path())
                    .display()
                    .to_string();
                format!("{relative}: {line_count} lines")
            })
        })
        .collect::<Vec<_>>();

    assert!(
        oversized.is_empty(),
        "Rust source files over {MAX_REASONABLE_SOURCE_LINES} lines should be split:\n{}",
        oversized.join("\n")
    );
}

#[test]
fn rust_schema_contract_test_files_stay_under_reasonable_size_limit() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut test_files = vec![manifest_dir.join("tests/tool_schema_contract.rs")];
    test_files.extend(collect_rust_files(
        &manifest_dir.join("tests/tool_schema_contract"),
    ));
    let oversized = test_files
        .into_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).expect("read test file");
            let line_count = content.lines().count();
            (line_count > MAX_REASONABLE_SOURCE_LINES).then(|| {
                let relative = path
                    .strip_prefix(manifest_dir)
                    .unwrap_or(path.as_path())
                    .display()
                    .to_string();
                format!("{relative}: {line_count} lines")
            })
        })
        .collect::<Vec<_>>();

    assert!(
        oversized.is_empty(),
        "Rust schema contract test files over {MAX_REASONABLE_SOURCE_LINES} lines should be split:\n{}",
        oversized.join("\n")
    );
}

#[test]
fn runtime_engine_root_stays_focused_on_loop_orchestration() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime_engine = manifest_dir.join("src/runtime/engine/mod.rs");
    let content = std::fs::read_to_string(&runtime_engine).expect("read runtime engine module");
    let line_count = content.lines().count();

    assert!(
        line_count <= MAX_REASONABLE_SOURCE_LINES,
        "runtime/engine/mod.rs is over {MAX_REASONABLE_SOURCE_LINES} lines and should be split before growing further; found {line_count} lines"
    );

    for module in [
        "completion",
        "construction",
        "controls",
        "cycle_inputs",
        "helpers",
        "logging",
        "memory",
        "planning",
        "run_setup",
        "state",
    ] {
        assert!(
            content.contains(&format!("mod {module};")),
            "runtime/engine/mod.rs should delegate {module} responsibilities to a focused submodule"
        );
    }
}
