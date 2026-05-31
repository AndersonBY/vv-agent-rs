#![allow(deprecated)]

use std::ffi::OsStr;

mod common;

use common::{env_string, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let image_path = env_string("V_AGENT_EXAMPLE_IMAGE_PATH", "test_image.png");
    let output_path = env_string(
        "V_AGENT_EXAMPLE_OUTPUT_PATH",
        "artifacts/image_read_report.md",
    );
    let image_file = config.workspace.join(&image_path);
    if !image_file.is_file() {
        let available = find_images(&config.workspace)?;
        return Err(
            format!("Image not found: {image_path}. Available images: {available:?}").into(),
        );
    }

    let prompt = format!(
        concat!(
            "请完成以下任务并严格执行:\n",
            "1) 调用 `read_image` 读取 `{}`.\n",
            "2) 基于图片内容生成中文 Markdown, 包含标题、场景概述、关键元素、可见文字和不确定推断。\n",
            "3) 调用 `write_file` 将 Markdown 覆盖写入 `{}`.\n",
            "4) 调用 `task_finish`, 最终 message 中包含输出文件路径。\n",
            "要求: 不要假装读图, 必须先调用 `read_image`."
        ),
        image_path, output_path
    );

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = "你是视觉理解助手, 你会读取图片并输出结构化 Markdown 分析.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.language = "zh-CN".to_string();
    agent.max_cycles = 12;
    agent.use_workspace = true;
    agent.native_multimodal = true;
    agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace.clone(),
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(prompt)?;
    print_run(&run)?;
    let output_file = config.workspace.join(output_path);
    if output_file.is_file() {
        println!(
            "\n[Generated Markdown]\n{}",
            std::fs::read_to_string(output_file)?
        );
    }
    Ok(())
}

fn find_images(root: &std::path::Path) -> Result<Vec<String>, std::io::Error> {
    let mut stack = vec![root.to_path_buf()];
    let mut images = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let suffix = path
                .extension()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(suffix.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp") {
                if let Ok(relative) = path.strip_prefix(root) {
                    images.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    images.sort();
    Ok(images)
}
