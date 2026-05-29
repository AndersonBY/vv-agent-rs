use super::ConfigError;

pub(super) fn backend_type_from_str(backend: &str) -> Result<vv_llm::BackendType, ConfigError> {
    match backend {
        "openai" => Ok(vv_llm::BackendType::OpenAI),
        "zhipuai" => Ok(vv_llm::BackendType::ZhiPuAI),
        "minimax" => Ok(vv_llm::BackendType::MiniMax),
        "moonshot" => Ok(vv_llm::BackendType::Moonshot),
        "anthropic" => Ok(vv_llm::BackendType::Anthropic),
        "mistral" => Ok(vv_llm::BackendType::Mistral),
        "deepseek" => Ok(vv_llm::BackendType::DeepSeek),
        "qwen" => Ok(vv_llm::BackendType::Qwen),
        "groq" => Ok(vv_llm::BackendType::Groq),
        "local" => Ok(vv_llm::BackendType::Local),
        "yi" => Ok(vv_llm::BackendType::Yi),
        "gemini" => Ok(vv_llm::BackendType::Gemini),
        "baichuan" => Ok(vv_llm::BackendType::Baichuan),
        "stepfun" => Ok(vv_llm::BackendType::StepFun),
        "xai" => Ok(vv_llm::BackendType::XAI),
        "xiaomi" => Ok(vv_llm::BackendType::Xiaomi),
        "ernie" => Ok(vv_llm::BackendType::Ernie),
        other => Err(ConfigError::UnsupportedBackend(other.to_string())),
    }
}
