//! crates/ramaria-desktop/src/commands/setup.rs - 首次配置 Tauri Commands
//!
//! 设计特点:
//! - run_setup: 执行完整的首次配置流程（保存配置 → 验证连接 → 初始化人格）
//! - get_setup_status: 返回当前配置状态的详细诊断
//! - refresh_setup_state: 刷新应用状态机，前端据此更新 UI
//! - 所有错误返回用户友好的中文描述
//! - init_default_personas: 创建 user-0001 + 扫描 personas/ 目录批量注册人格

use crate::DesktopState;
use serde::Serialize;
use std::path::Path;
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
#[tracing::instrument(skip(state, api_key, base_url))]
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

    // ---- 执行设置流程（密钥入 keychain → 配置落库与文件同步 → provider 热替换 → 状态推进） ----
    let request = ramaria_service::SetupRequest {
        provider: llm_provider,
        model_id: model_id.clone(),
        base_url: base_url.clone(),
        api_key,
    };
    state
        .engine
        .run_setup(&request)
        .await
        .map_err(|e| format!("设置流程失败: {}", e))?;

    // ---- ★ 初始化默认人格（user-0001 + 扫描 personas/ 目录） ----
    // 桌面端此前缺失此步骤，导致对话页/记忆页的人格选择器为空。
    // 对齐 CLI 的 create_initial_personas 行为。
    init_default_personas(&state.engine)
        .await
        .map_err(|e| format!("初始化人格失败: {}", e))?;

    // ---- 索引待构建时完成一次构建（缺索引的库），随后返回推进后的状态 ----
    crate::ensure_index_ready(&state.engine).await;
    let new_state = state.engine.current_state();

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
///
/// 接线状态（未接线/预留）:
/// - 前端设置页当前经 `refresh_setup_state` / `get_embedding_model` 获取状态，
///   未调用本命令；
/// - 保留该命令以提供更细的缺项诊断（`missing_items`），是否接入 UI 或下线由负责人裁定。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_setup_status(state: State<'_, DesktopState>) -> Result<SetupStatusView, String> {
    let status = state
        .engine
        .check_setup_status()
        .await
        .map_err(|e| format!("查询设置状态失败: {}", e))?;

    let current_state = state.engine.current_state();

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
        .engine
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
    // ★ 先取 provider 快照（Arc 克隆）再 await，避免持有锁跨 .await
    let llm = state.engine.llm();

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
/// - 校验不通过（目录缺失 / 加载失败 / 推理失败）以 valid=false + 原因返回，不抛错。
#[tauri::command]
#[tracing::instrument(skip(state, path))]
pub async fn validate_embedding_model(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<EmbeddingValidationResult, String> {
    let result = state
        .engine
        .validate_embedding_model(&path)
        .await
        .map_err(|e| format!("校验嵌入模型失败: {}", e))?;

    // 校验结果透传（valid=false + reason 表达失败原因）
    Ok(EmbeddingValidationResult {
        valid: result.valid,
        dimension: result.dimension,
        reason: result.reason,
    })
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
/// - path 非空时加载模型并热更新（路径持久化，下次启动自动加载）。
/// - path 为空时卸载嵌入模型。
/// - 加载失败时不修改内存 provider 与持久化配置（保持原状态下可用）。
#[tauri::command]
#[tracing::instrument(skip(state, path))]
pub async fn save_embedding_model(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<String, String> {
    // 目录存在性预检（空路径 = 卸载，不预检；保持既有错误文案）
    let path_trimmed = path.trim();
    if !path_trimmed.is_empty() && !Path::new(path_trimmed).exists() {
        return Err(format!("模型目录不存在: {}", path_trimmed));
    }

    state
        .engine
        .save_embedding_model(Some(path.as_str()))
        .await
        .map_err(|e| format!("加载嵌入模型失败: {}", e))?;

    tracing::info!("嵌入模型配置已持久化");
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
    let view = state
        .engine
        .embedding_model()
        .await
        .map_err(|e| format!("读取后端配置失败: {}", e))?;

    // 服务层视图字段与桌面视图一致，直接映射
    Ok(view.map(|v| EmbeddingModelView {
        model_path: v.model_path,
        valid: v.valid,
        dimension: v.dimension,
    }))
}

// ---- get_degraded_reason ----

/// 获取当前 Degraded 状态的详细原因。
///
/// 返回:
/// - `"embedding_missing"`: 嵌入模型缺失，向量搜索不可用。
/// - `"llm_unavailable"`: LLM provider 不可用。
/// - `"both_unavailable"`: LLM 与嵌入模型同时不可用。
/// - `"unknown"`: 其他未知原因。
/// - `null`: 当前未处于 Degraded 状态。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_degraded_reason(
    state: State<'_, DesktopState>,
) -> Result<Option<DegradedReason>, String> {
    let reason = state
        .engine
        .degraded_reason()
        .await
        .map_err(|e| format!("查询降级原因失败: {}", e))?;

    // 服务层原因分类映射为桌面视图枚举（序列化口径一致）
    Ok(reason.map(|reason| match reason {
        ramaria_service::DegradedReason::EmbeddingMissing => DegradedReason::EmbeddingMissing,
        ramaria_service::DegradedReason::LlmUnavailable => DegradedReason::LlmUnavailable,
        ramaria_service::DegradedReason::BothUnavailable => DegradedReason::BothUnavailable,
        ramaria_service::DegradedReason::Unknown => DegradedReason::Unknown,
        // non_exhaustive 兜底：未知分类按"其它原因"
        _ => DegradedReason::Unknown,
    }))
}

// =========================================================
// init_default_personas — 初始化默认人格
// =========================================================

/// 首次配置时创建默认人格记录。
///
/// 流程:
/// 1. 创建 `user-0001`（本地用户，如已存在则跳过）
/// 2. 扫描 `config/personas/` 目录并导入全部 `.toml` 文件（文件名=UID，内容=config）
/// 3. 目录无文件时尝试旧单文件路径 `config/persona.toml`（兼容回退）
///
/// 降级策略:
/// - 目录不存在 → 仅创建 user-0001，记录 warn 日志
/// - 单文件读取失败 → 跳过该文件，继续处理其他文件
/// - persona 已存在 → 跳过（幂等）
///
/// 路径解析（cargo tauri dev 从 crates/ramaria-desktop/ 运行）:
/// - 主路径: `../../config/personas` → workspace 根 `rust/config/personas/`
/// - 回退路径: `../config/personas`（兼容 workspace 根运行场景）
async fn init_default_personas(engine: &ramaria_service::Engine) -> Result<(), String> {
    // ---- Step 1: 确保 user-0001 存在（幂等） ----
    engine
        .persona_ensure_user()
        .await
        .map_err(|e| format!("创建 user-0001 失败: {}", e))?;

    // ---- Step 2: 目录解析（多级回退） ----
    let candidates = [
        Path::new("../../config/personas"),
        Path::new("../config/personas"),
    ];
    let dir = candidates.iter().find(|p| p.exists() && p.is_dir());
    let Some(dir) = dir else {
        tracing::warn!(
            candidates = ?candidates
                .iter()
                .map(|p| crate::path_guard::redact_path_label(p))
                .collect::<Vec<_>>(),
            "personas 目录不存在（已尝试所有候选路径）"
        );
        return Ok(());
    };

    tracing::info!(
        dir = %crate::path_guard::redact_path_label(dir),
        "扫描 personas 目录"
    );

    // ---- Step 3: 目录导入（服务层用例；已存在跳过，失败不阻塞） ----
    let outcomes = match engine
        .persona_load_from_dir(dir, None, ramaria_service::PersonaLoadMode::CreateMissing)
        .await
    {
        Ok(outcomes) => outcomes,
        Err(e) => {
            tracing::warn!(error = %e, "读取 personas 目录失败，跳过人格文件导入");
            return Ok(());
        }
    };
    if !outcomes.is_empty() {
        return Ok(());
    }

    // ---- Step 4: 旧单文件兼容回退（目录无 .toml 时） ----
    load_legacy_persona_file(engine).await
}

/// 旧单文件布局的兼容回退：`config/persona.toml` → `rama-0001`。
///
/// 说明:
/// - 仅在 `config/personas/` 目录存在但无 `.toml` 文件时调用；
/// - 旧文件不存在时给出"未找到人格文件"引导提示；
/// - 已存在的 `rama-0001` 跳过（幂等）；导入失败不阻塞启动流程。
async fn load_legacy_persona_file(engine: &ramaria_service::Engine) -> Result<(), String> {
    let old_path = Path::new("../../config/persona.toml");
    if !old_path.exists() {
        tracing::warn!("未找到人格文件。请将 .toml 文件放入 config/personas/ 目录");
        tracing::warn!("示例: config/personas/rama-0001.toml");
        return Ok(());
    }

    // 旧布局文件名不携带 uid：显式按 rama-0001 导入，名称缺失时回退 Ramaria
    let outcome = engine
        .persona_load_file(
            old_path,
            "rama-0001",
            "Ramaria",
            ramaria_service::PersonaLoadMode::CreateMissing,
        )
        .await;

    match outcome.action {
        ramaria_service::PersonaFileAction::Skipped => {
            tracing::debug!("rama-0001 已存在，跳过创建（旧单文件回退）");
        }
        ramaria_service::PersonaFileAction::Failed => {
            tracing::warn!(
                file = %crate::path_guard::redact_path_label(old_path),
                error = %outcome.message,
                "旧 persona.toml 加载失败（兼容回退，跳过）"
            );
        }
        _ => {
            tracing::info!(
                file = %crate::path_guard::redact_path_label(old_path),
                "从旧路径加载 persona.toml（兼容回退）"
            );
        }
    }
    Ok(())
}
