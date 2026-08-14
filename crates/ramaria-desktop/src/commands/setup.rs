//! rust/crates/ramaria-desktop/src/commands/setup.rs - 首次配置 Tauri Commands
//!
//! 设计特点:
//! - run_setup: 执行完整的首次配置流程（保存配置 → 验证连接 → 初始化人格）
//! - get_setup_status: 返回当前配置状态的详细诊断
//! - refresh_setup_state: 刷新应用状态机，前端据此更新 UI
//! - 所有错误返回用户友好的中文描述
//! - init_default_personas: 创建 user-0001 + 扫描 personas/ 目录批量注册人格

use crate::DesktopState;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{BackendConfig, Persona, PersonaKind};
use serde::Serialize;
use std::path::Path;
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
    pub embedding_available: bool,
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

    // ---- 构建后端配置（保留已保存的嵌入模型路径）----
    // 先读取当前配置中的 embedding_model_path，防止被 overwrite
    let existing_embedding_path = state
        .app
        .storage()
        .get_backend_config()
        .await
        .map_err(|e| format!("读取后端配置失败: {}", e))?
        .and_then(|c| c.embedding_model_path);

    let mut config =
        BackendConfig::new_with_defaults(llm_provider, base_url.clone(), model_id.clone());
    config.embedding_model_path = existing_embedding_path;

    state
        .app
        .storage()
        .save_backend_config(&config)
        .await
        .map_err(|e| format!("保存后端配置失败: {}", e))?;

    // ---- ★ 初始化默认人格（user-0001 + 扫描 personas/ 目录） ----
    // 桌面端此前缺失此步骤，导致对话页/记忆页的人格选择器为空。
    // 对齐 CLI 的 create_initial_personas 行为。
    init_default_personas(&state)
        .await
        .map_err(|e| format!("初始化人格失败: {}", e))?;

    // ---- 执行设置流程（含 LLM 连接验证） ----
    let new_state = state
        .app
        .run_setup(&config)
        .await
        .map_err(|e| format!("设置流程失败: {}", e))?;

    // ---- ★ 热更新 LLM provider，确保后续对话使用新配置 ----
    // 复用 App 持有的精确缓存实例（v1.5 C）：切换后端后缓存不失效
    let llm_cache = state.app.llm_cache();
    let new_llm: Arc<dyn ramaria_core::traits::LlmProvider> = match llm_provider {
        ramaria_core::types::LlmProvider::LmStudio => {
            let provider = ramaria_llm::lm_studio::LmStudioProvider::new(config.clone())
                .map_err(|e| format!("创建 LM Studio provider 失败: {}", e))?;
            let provider = match &llm_cache {
                Some(cache) => provider.with_cache(Arc::clone(cache)),
                None => provider,
            };
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::DeepSeek => {
            let provider = ramaria_llm::deepseek::DeepSeekProvider::new(
                config.clone(),
                state.app.keychain_arc(),
            )
            .map_err(|e| format!("创建 DeepSeek provider 失败: {}", e))?;
            let provider = match &llm_cache {
                Some(cache) => provider.with_cache(Arc::clone(cache)),
                None => provider,
            };
            Arc::new(provider)
        }
        ramaria_core::types::LlmProvider::OpenAI => {
            let provider =
                ramaria_llm::openai::OpenAIProvider::new(config.clone(), state.app.keychain_arc())
                    .map_err(|e| format!("创建 OpenAI provider 失败: {}", e))?;
            let provider = match &llm_cache {
                Some(cache) => provider.with_cache(Arc::clone(cache)),
                None => provider,
            };
            Arc::new(provider)
        }
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
    let embedding_available = state.app.is_embedding_available();

    let status = ramaria_app::setup::check_setup_status(storage.as_ref(), embedding_available)
        .await
        .map_err(|e| format!("查询设置状态失败: {}", e))?;

    let current_state = state.app.current_state();

    let view = SetupStatusView {
        backend_configured: status.backend_configured,
        model_selected: status.model_selected,
        needs_indexing: status.needs_indexing,
        embedding_available: status.embedding_available,
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

// =========================================================
// test_llm_connection — 测试 LLM 连接
// =========================================================

/// 测试 LLM 后端连接是否可达。
///
/// 说明:
/// - 此命令独立于应用状态机，仅测试 LLM provider 的 `validate` 方法。
/// - 与 `refresh_setup_state` 不同：不检查嵌入模型、不检查索引状态。
/// - 前端首次配置向导 Step 1 的「测试连接」按钮使用此命令。
///
/// 返回:
/// - `"ok"`: LLM 连接正常。
/// - 否则返回错误信息（含可操作的故障排查提示）。
///
/// 注意:
/// - LM Studio 场景：验证 base_url 可达 + /models 端点可访问。
/// - DeepSeek/OpenAI 场景：额外验证 API key 非空。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn test_llm_connection(state: State<'_, DesktopState>) -> Result<String, String> {
    // ★ 先 clone Arc 出锁再 await，避免 MutexGuard 跨 .await
    let llm = state.app.llm_clone();

    llm.validate()
        .await
        .map(|_| "ok".to_string())
        .map_err(|e| format!("LLM 连接测试失败: {}", e))
}

// =========================================================
// Embedding 模型命令
// =========================================================

/// 嵌入模型校验结果（前端展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingValidationResult {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 嵌入模型配置视图（前端展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingModelView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
}

/// 降级原因枚举。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedReason {
    EmbeddingMissing,
    LlmUnavailable,
    BothUnavailable,
    Unknown,
}

// ---- validate_embedding_model ----

/// 校验嵌入模型路径是否有效。
///
/// 参数:
/// - `path`: 模型文件夹绝对路径。
///
/// 返回:
/// - `EmbeddingValidationResult`: valid=true 且 dimension 有值表示校验通过。
///
/// 说明:
/// - 创建 NativeEmbeddingProvider 并调用 `validate` 方法。
/// - 若目录不存在或模型文件缺失，返回 valid=false + 原因说明。
#[tauri::command]
#[tracing::instrument]
pub async fn validate_embedding_model(path: String) -> Result<EmbeddingValidationResult, String> {
    let model_dir = Path::new(&path);

    // 检查目录是否存在
    if !model_dir.exists() {
        return Ok(EmbeddingValidationResult {
            valid: false,
            dimension: None,
            reason: Some(format!("模型目录不存在: {}", path)),
        });
    }

    if !model_dir.is_dir() {
        return Ok(EmbeddingValidationResult {
            valid: false,
            dimension: None,
            reason: Some(format!("路径不是目录: {}", path)),
        });
    }

    // 尝试创建 provider 并校验
    match ramaria_llm::embedding::native::create_native_provider(model_dir) {
        Ok(provider) => {
            let dim = provider.model_info().dimension;
            match provider.validate().await {
                Ok(()) => {
                    tracing::info!(path = %path, dimension = dim, "嵌入模型校验通过");
                    Ok(EmbeddingValidationResult {
                        valid: true,
                        dimension: Some(dim),
                        reason: None,
                    })
                }
                Err(e) => {
                    tracing::warn!(path = %path, error = %e, "嵌入模型校验失败");
                    Ok(EmbeddingValidationResult {
                        valid: false,
                        dimension: Some(dim),
                        reason: Some(format!("模型文件存在但推理失败: {}", e)),
                    })
                }
            }
        }
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "嵌入模型加载失败");
            Ok(EmbeddingValidationResult {
                valid: false,
                dimension: None,
                reason: Some(format!("模型加载失败: {}", e)),
            })
        }
    }
}

// ---- save_embedding_model ----

/// 保存嵌入模型配置并热加载到应用。
///
/// 参数:
/// - `path`: 模型文件夹绝对路径（空字符串表示卸载嵌入模型）。
///
/// 返回:
/// - `"ok"`: 保存成功。
///
/// 说明:
/// - path 非空时创建 NativeEmbeddingProvider 并通过 `app.update_embedding` 热加载。
/// - path 为空时卸载嵌入模型（传入 None）。
/// - 路径持久化到 BackendConfig（与 base_url 一致），下次启动自动加载。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn save_embedding_model(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<String, String> {
    let path_trimmed = path.trim();

    // ---- 读当前 BackendConfig ----
    let mut config = state
        .app
        .storage()
        .get_backend_config()
        .await
        .map_err(|e| format!("读取后端配置失败: {}", e))?
        .unwrap_or_else(BackendConfig::lm_studio_default);

    if path_trimmed.is_empty() {
        // 卸载嵌入模型
        state.app.update_embedding(None);
        config.embedding_model_path = None;
        tracing::info!("嵌入模型已卸载");
    } else {
        let model_dir = Path::new(path_trimmed);
        if !model_dir.exists() {
            return Err(format!("模型目录不存在: {}", path_trimmed));
        }

        // 创建 provider
        let provider = ramaria_llm::embedding::native::create_native_provider(model_dir)
            .map_err(|e| format!("加载嵌入模型失败: {}", e))?;

        tracing::info!(
            path = %path_trimmed,
            dimension = provider.model_info().dimension,
            "嵌入模型已加载，准备热更新"
        );

        let provider: Arc<dyn EmbeddingProvider> = Arc::new(provider);
        state.app.update_embedding(Some(provider));
        config.embedding_model_path = Some(path_trimmed.to_string());
    }

    // ---- 持久化到 BackendConfig（复用 LLM 的 save_backend_config 模式） ----
    state
        .app
        .storage()
        .save_backend_config(&config)
        .await
        .map_err(|e| format!("保存后端配置失败: {}", e))?;

    tracing::info!(
        path = %config.embedding_model_path.as_deref().unwrap_or("(无)"),
        "嵌入模型配置已持久化"
    );
    Ok("ok".to_string())
}

// ---- get_embedding_model ----

/// 获取当前嵌入模型配置。
///
/// 返回:
/// - `EmbeddingModelView | null`: 当前嵌入模型信息，未配置时返回 null。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_embedding_model(
    state: State<'_, DesktopState>,
) -> Result<Option<EmbeddingModelView>, String> {
    let provider_opt = state.app.embedding_provider();

    match provider_opt {
        Some(provider) => {
            let info = provider.model_info();
            let is_avail = provider.is_available();
            Ok(Some(EmbeddingModelView {
                model_path: None, // model_id 通常不暴露本地路径
                valid: is_avail,
                dimension: Some(info.dimension),
            }))
        }
        None => {
            // 从 BackendConfig 读取已保存路径（用于 UI 预填，复用 LLM base_url 模式）
            let config = state
                .app
                .storage()
                .get_backend_config()
                .await
                .map_err(|e| format!("读取后端配置失败: {}", e))?
                .unwrap_or_else(BackendConfig::lm_studio_default);

            match config.embedding_model_path {
                Some(saved_path) if !saved_path.is_empty() => Ok(Some(EmbeddingModelView {
                    model_path: Some(saved_path),
                    valid: false,
                    dimension: None,
                })),
                _ => Ok(None),
            }
        }
    }
}

// ---- get_degraded_reason ----

/// 获取当前 Degraded 状态的详细原因。
///
/// 返回:
/// - `"embedding_missing"`: 嵌入模型缺失，向量搜索不可用。
/// - `"llm_unavailable"`: LLM provider 不可用。
/// - `"unknown"`: 其他未知原因。
/// - `null`: 当前未处于 Degraded 状态。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_degraded_reason(
    state: State<'_, DesktopState>,
) -> Result<Option<DegradedReason>, String> {
    let current_state = state.app.current_state();

    if current_state != ramaria_core::types::AppState::Degraded {
        return Ok(None);
    }

    let embedding_ok = state.app.is_embedding_available();
    let llm = state.app.llm_clone();

    // 检查 LLM 是否可用
    let llm_ok = llm.validate().await.is_ok();

    match (llm_ok, embedding_ok) {
        (false, false) => Ok(Some(DegradedReason::BothUnavailable)),
        (false, true) => Ok(Some(DegradedReason::LlmUnavailable)),
        (true, false) => Ok(Some(DegradedReason::EmbeddingMissing)),
        _ => Ok(Some(DegradedReason::Unknown)),
    }
}

// =========================================================
// init_default_personas — 初始化默认人格
// =========================================================

/// 首次配置时创建默认人格记录。
///
/// 流程:
/// 1. 创建 `user-0001`（本地用户，如已存在则跳过）
/// 2. 扫描 `config/personas/` 目录下所有 `.toml` 文件
/// 3. 对每个文件创建对应的 persona 记录（文件名=UID，TOML内容=config）
///
/// 降级策略:
/// - 目录不存在 → 仅创建 user-0001，记录 warn 日志
/// - 单文件读取失败 → 跳过该文件，继续处理其他文件
/// - persona 已存在 → 跳过（幂等）
/// - assistant_name 提取失败 → 回退使用 UID 作为 name
///
/// 路径解析（cargo tauri dev 从 crates/ramaria-desktop/ 运行）:
/// - 主路径: `../../config/personas` → workspace 根 `rust/config/personas/`
/// - 回退路径: `../config/personas`（兼容 workspace 根运行场景）
async fn init_default_personas(state: &State<'_, DesktopState>) -> Result<(), String> {
    let storage = state.app.storage();

    // ---- Step 1: 确保 user-0001 存在 ----
    if storage
        .get_persona_by_uid("user-0001")
        .await
        .map_err(|e| format!("查询 user-0001 失败: {}", e))?
        .is_none()
    {
        let user = Persona::new(
            "user-0001".to_string(),
            "用户".to_string(),
            PersonaKind::User,
            1,
            "system".to_string(),
        );
        storage
            .create_persona(&user)
            .await
            .map_err(|e| format!("创建 user-0001 失败: {}", e))?;
        tracing::info!("已创建 persona: user-0001 (用户)");
    } else {
        tracing::debug!("user-0001 已存在，跳过创建");
    }

    // ---- Step 2: 扫描 personas/ 目录 ----
    let persona_entries = scan_personas_dir();
    if persona_entries.is_empty() {
        tracing::warn!("未找到人格文件。请将 .toml 文件放入 config/personas/ 目录");
        tracing::warn!("示例: config/personas/rama-0001.toml");
        return Ok(());
    }

    // ---- Step 3: 逐文件创建 persona ----
    for (uid, name, config_content) in persona_entries {
        // 幂等：已存在的 persona 跳过
        if storage
            .get_persona_by_uid(&uid)
            .await
            .map_err(|e| format!("查询 persona {} 失败: {}", uid, e))?
            .is_some()
        {
            tracing::debug!(%uid, "persona 已存在，跳过创建");
            continue;
        }

        let kind = PersonaKind::from_uid(&uid);
        let mut persona = Persona::new(uid.clone(), name.clone(), kind, 1, "file".to_string());
        persona.config = Some(config_content);

        storage
            .create_persona(&persona)
            .await
            .map_err(|e| format!("创建 persona {} 失败: {}", uid, e))?;

        tracing::info!(%uid, %name, "已创建 persona");
    }

    Ok(())
}

/// 扫描 `config/personas/` 目录，返回所有 `.toml` 文件的信息。
///
/// 返回:
/// - `Vec<(uid, assistant_name, raw_toml_content)>`
///
/// 路径策略（多级回退）:
/// - `../../config/personas`（cargo tauri dev 从 crates/ramaria-desktop/ 运行）
/// - `../config/personas`（兼容直接 cargo run 或 workspace 根运行）
fn scan_personas_dir() -> Vec<(String, String, String)> {
    // 多级路径回退：寻找 personas 目录
    let candidates = [
        Path::new("../../config/personas"),
        Path::new("../config/personas"),
    ];

    let dir = candidates.iter().find(|p| p.exists() && p.is_dir());
    let dir = match dir {
        Some(d) => d,
        None => {
            tracing::warn!(
                candidates = ?candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "personas 目录不存在（已尝试所有候选路径）"
            );
            return Vec::new();
        }
    };

    tracing::info!(dir = %dir.display(), "扫描 personas 目录");

    let mut results: Vec<(String, String, String)> = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%e, dir = %dir.display(), "读取 personas 目录失败");
            return results;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // 仅处理 .toml 文件
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            continue;
        }

        // 文件名（不含扩展名）= persona UID
        let uid = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => {
                tracing::warn!(path = %path.display(), "无法从文件名提取 UID，跳过");
                continue;
            }
        };

        // 读取文件内容
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(%e, path = %path.display(), "读取 persona 文件失败，跳过");
                continue;
            }
        };

        // 从 TOML 中提取 assistant_name（简单行解析，零外部依赖）
        let name = extract_toml_assistant_name(&content).unwrap_or_else(|| uid.clone());

        tracing::info!(%uid, %name, path = %path.display(), "发现人格文件");
        results.push((uid, name, content));
    }

    // 兼容回退：新目录无文件时尝试旧单文件路径
    if results.is_empty() {
        let old_path = Path::new("../../config/persona.toml");
        if old_path.exists() {
            match std::fs::read_to_string(old_path) {
                Ok(content) => {
                    let name = extract_toml_assistant_name(&content)
                        .unwrap_or_else(|| "Ramaria".to_string());
                    tracing::info!(%name, path = %old_path.display(), "从旧路径加载 persona.toml（兼容回退）");
                    results.push(("rama-0001".to_string(), name, content));
                }
                Err(e) => {
                    tracing::warn!(%e, path = %old_path.display(), "读取旧 persona.toml 失败");
                }
            }
        }
    }

    results
}

/// 从 TOML 内容中提取 `assistant_name` 字段值。
///
/// 说明:
/// - 使用行级简单解析，不引入 toml crate 依赖
/// - 支持 `assistant_name = "值"` 格式（允许值中含空格）
/// - 提取失败时返回 None，调用方回退使用 UID
fn extract_toml_assistant_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();

        // 跳过注释和空行
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }

        // 匹配 `assistant_name = "珊瑚菌"` 或 bare word
        if let Some(rest) = trimmed.strip_prefix("assistant_name") {
            let rest = rest.trim();
            if let Some(eq_pos) = rest.find('=') {
                let value = rest[eq_pos + 1..].trim();
                // 去除双引号或单引号包裹
                if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    return Some(value[1..value.len() - 1].to_string());
                }
                // bare word（无引号）
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}
