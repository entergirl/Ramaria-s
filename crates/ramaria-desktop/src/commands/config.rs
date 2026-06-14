//! rust/crates/ramaria-desktop/src/commands/config.rs - 配置管理 Tauri Commands
//!
//! 设计特点:
//! - get_backend_config / update_backend_config: 读写当前 LLM 后端配置
//! - API key 通过 keychain 管理，不在此处暴露明文
//! - get_settings / update_setting: 读写全局 key-value 设置
//! - 所有敏感操作通过 ramaria-app 的错误提示映射

use crate::DesktopState;
use ramaria_core::types::BackendConfig;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 后端配置视图（API key 仅返回遮罩版本：前 4 位 + ... + 后 4 位）。
///
/// 序列化约定：camelCase 以对齐前端 JS 访问（config.baseUrl / config.modelId）。
/// 默认 serde 序列化为 snake_case，但前端 JS 代码全部使用 camelCase，
/// 如不显式重命名会导致 `config.baseUrl` 始终为 undefined → 回退到默认值。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendConfigView {
    pub provider: String,
    pub model_id: String,
    pub base_url: String,
    pub supports_streaming: bool,
    pub supports_json_mode: bool,
    pub context_window: u32,
    pub max_output_tokens: u32,
    /// API key 遮罩版本，如 "sk-a...b1c2"；本地 provider 或读取失败时为 None
    pub api_key_masked: Option<String>,
}

/// 设置项视图。
#[derive(Debug, Clone, Serialize)]
pub struct SettingView {
    pub key: String,
    pub value: String,
}

// =========================================================
// get_backend_config — 获取当前后端配置
// =========================================================

/// 获取当前 LLM 后端的非敏感配置。
///
/// 返回:
/// - BackendConfigView（不含 API key 信息）
///
/// 安全约束:
/// - API key 不会在此返回中暴露
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_backend_config(
    state: State<'_, DesktopState>,
) -> Result<BackendConfigView, String> {
    // 从 storage 读取最新配置（而非启动时缓存的 in-memory 值），
    // 确保 setup / update_backend_config 后的配置能正确回显。
    let config = state
        .app
        .storage()
        .get_backend_config()
        .await
        .map_err(|e| format!("读取后端配置失败: {}", e))?
        .unwrap_or_else(ramaria_core::types::BackendConfig::lm_studio_default);

    // 读取并遮罩 API key（仅线上 provider 有此字段）
    let api_key_masked = if config.provider.is_online() {
        let service = match config.provider {
            ramaria_core::types::LlmProvider::DeepSeek => "deepseek",
            ramaria_core::types::LlmProvider::OpenAI => "openai",
            _ => "",
        };
        if service.is_empty() {
            None
        } else {
            state
                .app
                .keychain()
                .get_api_key(service)
                .ok()
                .flatten()
                .map(|key| mask_api_key(&key))
        }
    } else {
        None
    };

    let view = BackendConfigView {
        provider: config.provider.as_str().to_string(),
        model_id: config.capability.model_id.clone(),
        base_url: config.base_url.clone(),
        supports_streaming: config.capability.supports_streaming,
        supports_json_mode: config.capability.supports_json_mode,
        context_window: config.capability.context_window,
        max_output_tokens: config.capability.max_output_tokens,
        api_key_masked,
    };

    tracing::debug!(provider = %view.provider, "get_backend_config 完成");
    Ok(view)
}

/// 遮罩 API key：统一策略——显示前 3 和后 3 字符，中间 `****`。
///
/// 安全约束:
/// - 短密钥（≤6 字符）只显示首字符 + `****`。
/// - 不再泄露前缀长度信息（如 `sk-` 前缀在旧策略下可被推断）。
/// - 与 deepseek/openai provider 的日志遮蔽策略保持一致。
///
/// 示例:
/// - `"sk-abc123def456"` → `"sk-****456"`
/// - `"short"` → `"s****"`
fn mask_api_key(key: &str) -> String {
    let len = key.len();
    if len <= 6 {
        // 短密钥：仅首字符 + ****
        format!("{}****", &key[..1])
    } else {
        // 标准策略：前 3 + **** + 后 3
        format!("{}****{}", &key[..3], &key[len - 3..])
    }
}

// =========================================================
// update_backend_config — 更新后端配置
// =========================================================

/// 更新 LLM 后端配置。
///
/// 参数:
/// - `provider`: "LmStudio" | "DeepSeek" | "OpenAI"
/// - `model_id`: 模型标识
/// - `base_url`: API 基础地址
/// - `api_key`: 可选，线上 provider 的 API key（写入 keychain）
///
/// 返回:
/// - `"updated"` 表示配置已更新
///
/// 说明:
/// - provider 切换时需重新进行隐私确认
/// - api_key 不为空时写入 OS keychain，为空则跳过
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn update_backend_config(
    state: State<'_, DesktopState>,
    provider: String,
    model_id: String,
    base_url: String,
    api_key: Option<String>,
) -> Result<String, String> {
    let llm_provider = match provider.to_lowercase().as_str() {
        "lmstudio" | "lm_studio" => ramaria_core::types::LlmProvider::LmStudio,
        "deepseek" => ramaria_core::types::LlmProvider::DeepSeek,
        "openai" => ramaria_core::types::LlmProvider::OpenAI,
        other => return Err(format!("不支持的 provider: {}", other)),
    };

    // 构建新的 BackendConfig（使用统一构造器，消除重复）
    let new_config =
        BackendConfig::new_with_defaults(llm_provider, base_url.clone(), model_id.clone());

    // 保存到 storage
    state
        .app
        .storage()
        .save_backend_config(&new_config)
        .await
        .map_err(|e| format!("保存后端配置失败: {}", e))?;

    // 如果有 API key，写入 keychain
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            let service = match llm_provider {
                ramaria_core::types::LlmProvider::DeepSeek => "deepseek",
                ramaria_core::types::LlmProvider::OpenAI => "openai",
                _ => {
                    tracing::warn!("本地 provider 不需要 API key，跳过写入");
                    "" // 返回空串，下方 is_empty() 检查会跳过写入
                }
            };
            if !service.is_empty() {
                state
                    .app
                    .keychain()
                    .set_api_key(service, &key)
                    .map_err(|e| format!("保存 API key 到 keychain 失败: {}", e))?;
                tracing::info!(provider = %provider, "API key 已写入 keychain");
            }
        }
    }

    // ★ 热更新内存中的 LLM provider，确保后续对话使用新配置
    let new_llm: Arc<dyn ramaria_core::traits::LlmProvider> = match llm_provider {
        ramaria_core::types::LlmProvider::LmStudio => Arc::new(
            ramaria_llm::lm_studio::LmStudioProvider::new(new_config.clone())
                .map_err(|e| format!("创建 LM Studio provider 失败: {}", e))?,
        ),
        ramaria_core::types::LlmProvider::DeepSeek => Arc::new(
            ramaria_llm::deepseek::DeepSeekProvider::new(
                new_config.clone(),
                state.app.keychain_arc(),
            )
            .map_err(|e| format!("创建 DeepSeek provider 失败: {}", e))?,
        ),
        ramaria_core::types::LlmProvider::OpenAI => Arc::new(
            ramaria_llm::openai::OpenAIProvider::new(new_config.clone(), state.app.keychain_arc())
                .map_err(|e| format!("创建 OpenAI provider 失败: {}", e))?,
        ),
        _ => return Err(format!("不支持的 provider: {}", provider)),
    };
    state.app.update_llm(new_llm);

    tracing::info!(provider = %provider, model_id = %model_id, "后端配置已更新并热加载");
    Ok("updated".to_string())
}

// =========================================================
// get_settings — 获取所有设置
// =========================================================

/// 获取所有全局设置项。
///
/// 返回:
/// - JSON 数组，每项为 SettingView（key-value 对）
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_settings(state: State<'_, DesktopState>) -> Result<Vec<SettingView>, String> {
    let settings = state
        .app
        .storage()
        .list_settings()
        .await
        .map_err(|e| format!("查询设置失败: {}", e))?;

    // list_settings 返回 Vec<(String, String)>
    let views: Vec<SettingView> = settings
        .into_iter()
        .map(|(key, value)| SettingView { key, value })
        .collect();

    tracing::debug!(count = views.len(), "get_settings 完成");
    Ok(views)
}

// =========================================================
// update_setting — 更新单个设置
// =========================================================

/// 更新或创建单个全局设置项。
///
/// 参数:
/// - `key`: 设置键名
/// - `value`: 设置值
///
/// 返回:
/// - `"updated"` 表示设置已保存
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn update_setting(
    state: State<'_, DesktopState>,
    key: String,
    value: String,
) -> Result<String, String> {
    if key.trim().is_empty() {
        return Err("设置键名不能为空".to_string());
    }

    state
        .app
        .storage()
        .set_setting(&key, &value)
        .await
        .map_err(|e| format!("保存设置失败: {}", e))?;

    tracing::info!(key = %key, "设置已更新");
    Ok("updated".to_string())
}
