use crate::prompt::templates;
use chrono::{SecondsFormat, Utc};

use super::options::BuildSystemPromptOptions;
use super::section::{PromptBundle, PromptSection};
use super::system_builder::SystemPromptBuilder;

pub fn build_system_prompt(original_system_prompt: impl Into<String>) -> String {
    build_system_prompt_bundle(original_system_prompt).flatten()
}

pub fn build_system_prompt_with_options(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> String {
    build_system_prompt_bundle_with_options(original_system_prompt, options).flatten()
}

pub fn build_system_prompt_sections(
    original_system_prompt: impl Into<String>,
) -> Vec<PromptSection> {
    build_system_prompt_bundle(original_system_prompt).sections
}

pub fn build_system_prompt_sections_with_options(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> Vec<PromptSection> {
    build_system_prompt_bundle_with_options(original_system_prompt, options).sections
}

pub fn build_system_prompt_bundle(original_system_prompt: impl Into<String>) -> PromptBundle {
    build_system_prompt_bundle_with_options(
        original_system_prompt,
        BuildSystemPromptOptions::default(),
    )
}

pub fn build_system_prompt_bundle_with_options(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> PromptBundle {
    create_system_prompt_builder(original_system_prompt, options).build_result()
}

pub fn create_system_prompt_builder(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> SystemPromptBuilder {
    let language = options.language;
    let mut builder = SystemPromptBuilder::default();
    builder.add_section(
        PromptSection::new(
            "agent_definition",
            format!(
                "<Agent Definition>\n{}\n</Agent Definition>",
                original_system_prompt.into()
            ),
            true,
        )
        .source("agent.instructions"),
    );

    if options.agent_type.as_deref() == Some("computer") {
        builder.add_section(
            PromptSection::new(
                "environment",
                format!(
                    "<Environment>\n{}\n</Environment>",
                    templates::computer_agent_env_prompt(&language)
                ),
                true,
            )
            .source("runtime.environment"),
        );
    }

    let mut tools_lines = Vec::new();
    if options.allow_interruption {
        tools_lines.push(templates::ask_user_prompt(&language).to_string());
    }
    if options.use_workspace {
        tools_lines.push(templates::tool_priority_prompt(&language).to_string());
    }
    if options.enable_todo_management {
        tools_lines.push(templates::todo_prompt(&language).to_string());
    }
    if !options.available_sub_agents.is_empty() {
        tools_lines.push(templates::render_sub_agents(
            &language,
            &options.available_sub_agents,
        ));
    }
    if let Some(available_skills) = options.available_skills.as_ref() {
        let skills_prompt = templates::render_available_skills(
            &language,
            available_skills,
            options.workspace.as_deref(),
        );
        if !skills_prompt.is_empty() {
            tools_lines.push(skills_prompt);
        }
    }
    tools_lines.push(templates::task_finish_prompt(&language).to_string());
    builder.add_section(
        PromptSection::new(
            "tools",
            format!("<Tools>\n{}\n</Tools>", tools_lines.join("\n\n")),
            true,
        )
        .source("runtime.tools"),
    );

    if options.session_memory_enabled && !options.session_memory_context.trim().is_empty() {
        builder.add_section(
            PromptSection::new("session_memory", options.session_memory_context, false)
                .source("session.memory"),
        );
    }

    let current_time = options.current_time_utc.unwrap_or_else(current_utc_text);
    builder.add_section(
        PromptSection::new(
            "current_time",
            format!(
                "<Current Time>\n{}\n{}\n</Current Time>",
                templates::current_time_prompt(&language),
                current_time
            ),
            false,
        )
        .source("run.clock"),
    );
    builder
}

pub fn build_raw_system_prompt_sections(system_prompt: impl Into<String>) -> Vec<PromptSection> {
    let text = system_prompt.into().trim().to_string();
    if text.is_empty() {
        return Vec::new();
    }
    vec![PromptSection::new("raw_system_prompt", text, true)]
}

fn current_utc_text() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
