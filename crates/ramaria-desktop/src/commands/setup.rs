//! rust/crates/ramaria-desktop/src/commands/setup.rs - 首次配置 Tauri Commands
//!
//! 设计特点:
//! - run_setup: 执行完整的首次配置流程（保存配置 → 验证连接 → 初始化人格）
//! - get_setup_status: 返回当前配置状态的详细诊断
//! - refresh_setup_state: 刷新应用状态机，前端据此更新 UI
//! - 所有错误返回用户友好的中文描述

use crate::DesktopState;
use ramaria_core::types::BackendConfig;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

// =========================================================
// 前端展示用结构体
// =========================================================

/// 设置状态视图。
#[derive(Debug, Clone, Serialize)]
pub struct SetupStatusView {
    pub backend_configured: bool,
    pub model_selected: bool,
    pub needs_indexing: bool,
    pub is_complete: bool,
    pub missing_items: Vec<String>,
    pub current_state: String,
}

// =========================================================
// run_setup — 执行首次配置
// =========================================================

/// 执行首次配置和 LLM 连接验证。
///
/// 参数:
/// - `provider`: "LmStudio" | "DeepSeek" | "OpenAI"
/// - `model_id`: 模型标识（LM Studio 可为空）
/// - `base_url`: API 基础地址
/// - `api_key`: 可选，线上 provider 的 API key
///
/// 返回:
/// - `"setup_complete"` 表示配置成功，应用进入 Ready 状态
/// - 如果 LLM 验证失败，返回错误信息
///
/// 说明:
/// - LM Studio 场景下 model_id 可为空（用户后续在 LM Studio 中选模型）
/// - DeepSeek/OpenAI 场景下 api_key 必填
/// - 配置保存后自动初始化默认人格（rama-0001）
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn run_setup(
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

    // ---- 线上 provider 必须提供 API key ----
    if llm_provider.is_online() {
        let key = api_key.as_deref().unwrap_or("");
        if key.trim().is_empty() {
            return Err(format!("{} 需要 API key，请填写后重试", provider));
        }
    }

    // ---- 保存 API key 到 keychain ----
    if let Some(ref key) = api_key {
        if !key.trim().is_empty() && llm_provider.is_online() {
            let service = match llm_provider {
                ramaria_core::types::LlmProvider::DeepSeek => "deepseek",
                ramaria_core::types::LlmProvider::OpenAI => "openai",
                _ => unreachable!(),
            };
            state
                .app
                .keychain()
                .set_api_key(service, key)
                .map_err(|e| format!("保存 API key 到 keychain 失败: {}", e))?;
            tracing::info!(provider = %provider, "API key 已写入 keychain");
        }
    }

    // ---- 构建并保存后端配置（使用统一构造器，消除重复）----
    let config = BackendConfig::new_with_defaults(llm_provider, base_url.clone(), model_id.clone());

    state
        .app
        .storage()
        .save_backend_config(&config)
        .await
        .map_err(|e| format!("保存后端配置失败: {}", e))?;

    // ---- 执行设置流程（含 LLM 连接验证） ----
    let new_state = state
        .app
        .run_setup(&config)
        .await
        .map_err(|e| format!("设置流程失败: {}", e))?;

    // ---- ★ 热更新 LLM provider，确保后续对话使用新配置 ----
    let new_llm: Arc<dyn ramaria_core::traits::LlmProvider> = match llm_provider {
        ramaria_core::types::LlmProvider::LmStudio => Arc::new(
            ramaria_llm::lm_studio::LmStudioProvider::new(config.clone())
                .map_err(|e| format!("创建 LM Studio provider 失败: {}", e))?,
        ),
        ramaria_core::types::LlmProvider::DeepSeek => Arc::new(
            ramaria_llm::deepseek::DeepSeekProvider::new(config.clone(), state.app.keychain_arc())
                .map_err(|e| format!("创建 DeepSeek provider 失败: {}", e))?,
        ),
        ramaria_core::types::LlmProvider::OpenAI => Arc::new(
            ramaria_llm::openai::OpenAIProvider::new(config.clone(), state.app.keychain_arc())
                .map_err(|e| format!("创建 OpenAI provider 失败: {}", e))?,
        ),
        _ => return Err(format!("不支持的 provider: {}", provider)),
    };
    state.app.update_llm(new_llm);

    tracing::info!(
        provider = %provider,
        model_id = %model_id,
        new_state = %new_state.as_str(),
        "首次配置完成，LLM provider 已热加载"
    );

    Ok(format!("setup_complete:{}", new_state.as_str()))
}

// =========================================================
// get_setup_status — 查询设置状态
// =========================================================

/// 查询当前应用设置状态的详细信息。
///
/// 返回:
/// - SetupStatusView，包含各配置项完成情况和缺失项列表
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_setup_status(state: State<'_, DesktopState>) -> Result<SetupStatusView, String> {
    let storage = state.app.storage();

    let status = ramaria_app::setup::check_setup_status(storage.as_ref())
        .await
        .map_err(|e| format!("查询设置状态失败: {}", e))?;

    let current_state = state.app.current_state();

    let view = SetupStatusView {
        backend_configured: status.backend_configured,
        model_selected: status.model_selected,
        needs_indexing: status.needs_indexing,
        is_complete: status.is_complete(),
        missing_items: status
            .missing_items()
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        current_state: current_state.as_str().to_string(),
    };

    tracing::debug!(
        is_complete = view.is_complete,
        state = %view.current_state,
        "get_setup_status 完成"
    );
    Ok(view)
}

// =========================================================
// refresh_setup_state — 刷新应用状态
// =========================================================

/// 刷新应用状态机（从 storage 重新读取配置并判定状态）。
///
/// 返回:
/// - 新的应用状态字符串
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn refresh_setup_state(state: State<'_, DesktopState>) -> Result<String, String> {
    let new_state = state
        .app
        .refresh_setup_state()
        .await
        .map_err(|e| format!("刷新状态失败: {}", e))?;

    tracing::info!(new_state = %new_state.as_str(), "应用状态已刷新");
    Ok(new_state.as_str().to_string())
}
