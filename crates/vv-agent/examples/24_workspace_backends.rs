use std::sync::Arc;

mod common;

use common::{
    build_direct_runtime, env_string, make_task_id, print_agent_result, runtime_log_handler,
    ExampleConfig,
};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{
    AgentTask, FileInfo, LocalWorkspaceBackend, MemoryWorkspaceBackend, RuntimeRunControls,
    S3WorkspaceBackend, S3WorkspaceConfig, WorkspaceBackend,
};

#[derive(Clone)]
struct PrefixedBackend {
    inner: Arc<dyn WorkspaceBackend>,
    prefix: String,
}

impl WorkspaceBackend for PrefixedBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        self.inner.list_files(base, glob)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        self.inner.read_text(path)
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.inner.read_bytes(path)
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let tagged = if append {
            content.to_string()
        } else {
            format!("{}{}", self.prefix, content)
        };
        self.inner.write_text(path, &tagged, append)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        self.inner.file_info(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn is_file(&self, path: &str) -> bool {
        self.inner.is_file(path)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        self.inner.mkdir(path)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let mode = env_string("V_AGENT_EXAMPLE_WS_MODE", "all").to_ascii_lowercase();
    let (runtime, resolved) = build_direct_runtime(&config, 90.0)?;
    let system_prompt = build_system_prompt_with_options(
        "You are a helpful agent. Use workspace tools to complete tasks.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            allow_interruption: true,
            use_workspace: true,
            ..BuildSystemPromptOptions::default()
        },
    );

    if matches!(mode.as_str(), "all" | "default") {
        run_backend_demo(
            "方式 1: 默认 LocalWorkspaceBackend",
            &runtime,
            &config,
            &resolved.model_id,
            &system_prompt,
            None,
            "在 workspace 中创建 hello.txt 写入 'Hello from default backend', 然后读取并输出内容。",
        )?;
    }
    if matches!(mode.as_str(), "all" | "memory") {
        run_backend_demo(
            "方式 2: MemoryWorkspaceBackend",
            &runtime,
            &config,
            &resolved.model_id,
            &system_prompt,
            Some(Arc::new(MemoryWorkspaceBackend::default())),
            "在 workspace 中创建 memo.txt 写入 'Hello from memory backend', 然后读取并输出内容。",
        )?;
        if !config.workspace.join("memo.txt").exists() {
            println!("  验证通过: memo.txt 未落盘");
        }
    }
    if matches!(mode.as_str(), "all" | "s3") {
        let bucket = std::env::var("S3_BUCKET").unwrap_or_default();
        if bucket.is_empty() {
            if mode == "s3" {
                return Err("S3 mode requires S3_BUCKET".into());
            }
            eprintln!("[跳过] 方式 3: S3 - 未设置 S3_BUCKET");
        } else {
            let mut s3_config = S3WorkspaceConfig::new(bucket);
            s3_config.prefix = std::env::var("S3_PREFIX").unwrap_or_default();
            s3_config.endpoint_url = std::env::var("S3_ENDPOINT_URL").ok();
            s3_config.region_name = std::env::var("S3_REGION").ok();
            s3_config.aws_access_key_id = std::env::var("S3_ACCESS_KEY_ID").ok();
            s3_config.aws_secret_access_key = std::env::var("S3_SECRET_ACCESS_KEY").ok();
            s3_config.addressing_style = env_string("S3_ADDRESSING_STYLE", "virtual");
            run_backend_demo(
                "方式 3: S3WorkspaceBackend",
                &runtime,
                &config,
                &resolved.model_id,
                &system_prompt,
                Some(Arc::new(S3WorkspaceBackend::from_config(s3_config)?)),
                "在 workspace 中创建 s3_test.txt 写入 'Hello from S3 backend', 然后读取并输出内容。",
            )?;
        }
    }
    if matches!(mode.as_str(), "all" | "custom") {
        let prefixed = PrefixedBackend {
            inner: Arc::new(LocalWorkspaceBackend::new(config.workspace.clone())),
            prefix: "[AUTO-TAG] ".to_string(),
        };
        run_backend_demo(
            "方式 4: PrefixedBackend 自定义装饰器后端",
            &runtime,
            &config,
            &resolved.model_id,
            &system_prompt,
            Some(Arc::new(prefixed)),
            "在 workspace 中创建 tagged.txt 写入 'custom backend works', 然后读取并输出内容。",
        )?;
        let tagged = config.workspace.join("tagged.txt");
        if tagged.exists() {
            println!("  磁盘内容: {:?}", std::fs::read_to_string(tagged)?);
        }
    }
    Ok(())
}

fn run_backend_demo(
    label: &str,
    runtime: &vv_agent::AgentRuntime<vv_agent::VvLlmClient>,
    config: &ExampleConfig,
    model_id: &str,
    system_prompt: &str,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    prompt: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n============================================================\n[demo] {label}");
    let mut task = AgentTask::new(
        make_task_id("ws_backend"),
        model_id.to_string(),
        system_prompt.to_string(),
        prompt,
    );
    task.max_cycles = 5;
    let result = runtime.run_with_controls(
        task,
        RuntimeRunControls {
            workspace: Some(config.workspace.clone()),
            workspace_backend,
            log_handler: runtime_log_handler(config.verbose),
            ..RuntimeRunControls::default()
        },
    )?;
    print_agent_result(&result)
}
