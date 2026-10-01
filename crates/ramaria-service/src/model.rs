//! crates/ramaria-service/src/model.rs - 模型管理用例（LLM 后端与嵌入模型热更新）
//!
//! 设计特点:
//! - 后端配置写入单一入口：显式选项控制 keychain / 落库 / provider 热替换 / 文件侧同步，
//!   配置落库为真相源；写入结果汇总各步骤完成情况与失败原因（文件侧失败只记日志，不阻塞）
//! - 热替换即生效：重建 provider 沿用引擎持有的精确缓存实例，切换后端后既有缓存不失效，
//!   后续对话与记忆加工立即使用新 provider，无需重启进程
//! - 嵌入模型按路径加载：校验 / 保存 / 读取 / 卸载四个动作覆盖设置页全部交互，
//!   卸载（空路径）与加载走同一用例，避免"只改配置不换实例"的半生效状态
//! - 降级不阻塞：嵌入模型缺失 / 不可用只影响向量通道（BM25 + 关键词镜像继续工作），
//!   用例不因嵌入缺失返回错误
//! - 校验可解释：目录缺失 / 加载失败 / 推理失败三类原因都以文本回传，
//!   设置页直接展示，不靠日志排查
//! - 模型文件管理：根目录解析 / 列表 / 就绪与体积查询 / 删除 / 下载均委派
//!   `ramaria-llm` 模型管理器，本层只做编排与错误分类
//! - 安全约束：API key 只经 OS keychain 读写（本地 provider 跳过），日志不记密钥内容

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{AppState, BackendConfig, LlmProvider};
use ramaria_llm::model_manager::{ModelManager, ProgressCallback};

use crate::engine::{Engine, build_llm_provider};
use crate::types::{DegradedReason, EmbeddingModelView, EmbeddingValidation};

// =========================================================
// LLM 后端配置更新
// =========================================================

/// 后端配置写入选项（显式控制各步骤是否执行）。
///
/// 职责:
/// - 让调用方按场景组合写入步骤（如仅落库 + 文件同步，或仅热替换），避免"写库顺带热替换"的隐性语义。
///
/// 字段约定:
/// - `api_key`: 有值且为线上 provider 时写入 keychain；`None` 表示不更新密钥；
/// - `hot_swap`: 是否重建 provider 并热替换引擎 LLM 快照；
/// - `sync_file`: 是否同步 config.toml 的 `[backend]` 组。
#[derive(Debug, Clone)]
pub struct BackendConfigWriteOptions {
    /// 线上 provider 的新密钥（None = 不更新）
    pub api_key: Option<String>,
    /// 是否重建 provider 并热替换（false = 跳过该步骤）
    pub hot_swap: bool,
    /// 是否同步 config.toml 的 [backend] 组
    pub sync_file: bool,
}

/// 后端配置写入结果（各步骤完成情况）。
///
/// 字段约定:
/// - `db_ok`: 配置是否已落库（真相源）；
/// - `provider_updated`: 是否已重建并热替换 provider（未执行热替换时为 false）；
/// - `file_ok`: config.toml 的 `[backend]` 组是否同步成功（未执行同步或失败时为 false）；
/// - `failures`: 未成功步骤的失败原因（不含密钥内容）。
#[derive(Debug, Clone)]
pub struct BackendConfigWriteOutcome {
    /// 配置是否已落库
    pub db_ok: bool,
    /// 是否已重建并热替换 provider
    pub provider_updated: bool,
    /// config.toml [backend] 组是否同步成功
    pub file_ok: bool,
    /// 失败原因（不含密钥内容）
    pub failures: Vec<String>,
}

/// 写入 LLM 后端配置（按显式选项执行：keychain → 落库 → 热替换 → 文件同步）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `config`: 新的后端配置（provider / base_url / model / 嵌入路径等）；
/// - `options`: 写入选项（各步骤开关，见 [`BackendConfigWriteOptions`]）。
///
/// 返回:
/// - `Ok(outcome)`: 配置已落库（`db_ok=true`）；文件侧同步失败不改变成功语义，
///   只记录日志并汇总到 `failures`（见 [`BackendConfigWriteOutcome`]）；
/// - `Err`: 密钥写入失败返回 `Privacy`；配置落库失败返回 `Storage`；
///   热替换时 provider 构造失败返回对应错误（此时配置已落库，下次启动装配按新配置重试）。
///
/// 说明:
/// - 步骤顺序固定：先写密钥再落配置，避免"配置指向无密钥后端"的中间态；
/// - 热替换失败时不再执行文件同步（与"配置已落库、provider 未替换"的中间态一致）；
/// - 本地 provider（LM Studio）不需要 API key，传入密钥只记 debug 并跳过。
pub(crate) async fn write_backend_config(
    engine: &Engine,
    config: &BackendConfig,
    options: &BackendConfigWriteOptions,
) -> RamariaResult<BackendConfigWriteOutcome> {
    // ---- 1. API key（仅线上 provider；本地 provider 无需密钥） ----
    let key = options
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(key) = key {
        if config.provider.is_online() {
            let service = keychain_service(config.provider)?;
            engine.keychain().set_api_key(service, key)?;
            tracing::info!(provider = %config.provider, "API key 已写入 keychain");
        } else {
            tracing::debug!(
                provider = %config.provider,
                "本地 provider 不需要 API key，跳过 keychain 写入"
            );
        }
    }

    // ---- 2. 后端配置落库（DB 为真相源） ----
    engine.storage_ref().save_backend_config(config).await?;
    let mut outcome = BackendConfigWriteOutcome {
        db_ok: true,
        provider_updated: false,
        file_ok: false,
        failures: Vec::new(),
    };

    // ---- 3. 重建 provider 并热替换（复用精确缓存，保证切换后端后缓存不失效） ----
    if options.hot_swap {
        let keychain = engine.keychain_arc();
        let provider = build_llm_provider(config, &keychain, engine.llm_cache())?;
        engine.update_llm(provider);
        outcome.provider_updated = true;
    }

    // ---- 4. 文件侧同步（尽力而为：保持 config.toml 的 [backend] 组与表一致） ----
    if options.sync_file {
        match engine.sync_backend_config(config).await {
            Ok(result) => {
                outcome.file_ok = result.file_ok;
                outcome.failures.extend(result.failures);
                if !outcome.file_ok {
                    tracing::warn!(
                        failures = outcome.failures.len(),
                        "后端配置已落库，但 config.toml 同步失败（下次加载校验以文件为准）"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "后端配置已落库，但 config.toml 同步失败（降级不阻塞）"
                );
                outcome.failures.push(format!("config.toml 同步失败: {e}"));
            }
        }
    }

    tracing::info!(
        provider = %config.provider,
        model = %config.capability.model_id,
        base_url = %config.base_url,
        db_ok = outcome.db_ok,
        provider_updated = outcome.provider_updated,
        file_ok = outcome.file_ok,
        "后端配置写入完成"
    );
    Ok(outcome)
}

/// 更新 LLM 后端配置并热加载新 provider（完整写入：密钥 → 落库 → 热替换 → 文件同步）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `config`: 新的后端配置（provider / base_url / model / 嵌入路径等）；
/// - `api_key`: 可选的线上 provider 密钥；`None` 或空白表示不更新密钥（沿用 keychain 现值）。
///
/// 返回:
/// - 成功时返回 `Ok(())`，此后读取路径取到的是新 provider；
/// - 错误语义与 `write_backend_config` 一致；文件侧同步失败只记日志，不改变成功语义。
///
/// 说明:
/// - 本函数是 `write_backend_config` 的完整选项薄包装；
///   只需"落库 + 文件同步"或只需热替换的调用方应直接使用 `write_backend_config` 并给出显式选项。
pub(crate) async fn update_backend_config(
    engine: &Engine,
    config: &BackendConfig,
    api_key: Option<&str>,
) -> RamariaResult<()> {
    let options = BackendConfigWriteOptions {
        api_key: api_key.map(str::to_string),
        hot_swap: true,
        sync_file: true,
    };
    write_backend_config(engine, config, &options).await?;
    Ok(())
}

/// 线上 provider 对应的 keychain 服务标识。
///
/// 返回:
/// - `deepseek` / `openai`；本地 provider 返回 `Validation` 错误（调用方已按线上分支过滤）。
fn keychain_service(provider: LlmProvider) -> RamariaResult<&'static str> {
    match provider {
        LlmProvider::DeepSeek => Ok("deepseek"),
        LlmProvider::OpenAI => Ok("openai"),
        other => Err(RamariaError::validation(format!(
            "provider {} 不使用 keychain 密钥",
            other.as_str()
        ))),
    }
}

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}

// =========================================================
// 嵌入模型校验 / 保存 / 读取
// =========================================================

/// 校验指定目录能否作为嵌入模型使用。
///
/// 参数:
/// - `path`: 模型文件夹路径（用户从设置页选择）。
///
/// 返回:
/// - 成功时返回 [`EmbeddingValidation`]：`valid` 表达结论，`reason` 表达原因。
/// - 目录缺失 / 不是目录 / 模型加载失败 / 推理失败均以 `valid=false` 返回（不抛错）。
///
/// 说明:
/// - 推理可用性检查会真实执行一次前向计算（`validate`），因此校验通过即"可用"；
/// - 该用例不修改引擎状态，仅做无副作用探测。
pub async fn validate_embedding_model(
    path: &str,
    device: ramaria_core::config::EmbeddingDevice,
) -> RamariaResult<EmbeddingValidation> {
    let model_dir = Path::new(path);

    if !model_dir.exists() {
        tracing::warn!(path = %path_log_label(model_dir), "嵌入模型校验：目录不存在");
        return Ok(EmbeddingValidation::invalid(format!(
            "模型目录不存在: {path}"
        )));
    }
    if !model_dir.is_dir() {
        tracing::warn!(path = %path_log_label(model_dir), "嵌入模型校验：路径不是目录");
        return Ok(EmbeddingValidation::invalid(format!(
            "路径不是目录: {path}"
        )));
    }

    match ramaria_llm::embedding::native::create_native_provider_with_device(model_dir, device) {
        Ok(provider) => {
            let dimension = provider.model_info().dimension;
            match provider.validate().await {
                Ok(()) => {
                    tracing::info!(path = %path_log_label(model_dir), dimension, "嵌入模型校验通过");
                    Ok(EmbeddingValidation {
                        valid: true,
                        dimension: Some(dimension),
                        reason: None,
                    })
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path_log_label(model_dir),
                        error = %e,
                        "嵌入模型校验失败（模型可加载但推理失败）"
                    );
                    Ok(EmbeddingValidation {
                        valid: false,
                        dimension: Some(dimension),
                        reason: Some(format!("模型文件存在但推理失败: {e}")),
                    })
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %path_log_label(model_dir),
                error = %e,
                "嵌入模型校验失败（模型加载失败）"
            );
            Ok(EmbeddingValidation {
                valid: false,
                dimension: None,
                reason: Some(format!("模型加载失败: {e}")),
            })
        }
    }
}

/// 保存嵌入模型配置并热加载（空路径 = 卸载）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `path`: 模型文件夹路径；`None` 或空白表示卸载当前嵌入模型。
///
/// 返回:
/// - 成功时返回 `Ok(())`：内存 provider 已替换，且 `backend_config.embedding_model_path`
///   已持久化（下次启动按此路径恢复）。
/// - 路径不存在返回 `Validation`；模型加载失败返回对应错误；配置落库失败返回 `Storage`。
///
/// 说明:
/// - 加载失败时不修改内存 provider 与持久化配置（保持原状态下可用，避免半生效）；
/// - 热更新后既有内存索引仍按旧向量构建：卸载 / 换模型后首次召回按懒加载策略刷新
///   （与"跨进程写入后刷新"同一路径）。
pub(crate) async fn save_embedding_model(engine: &Engine, path: Option<&str>) -> RamariaResult<()> {
    let mut config = engine
        .storage_ref()
        .get_backend_config()
        .await?
        .unwrap_or_else(BackendConfig::lm_studio_default);

    match path.map(str::trim).filter(|value| !value.is_empty()) {
        // ---- 卸载 ----
        None => {
            engine.update_embedding(None);
            config.embedding_model_path = None;
        }
        // ---- 加载并热更新 ----
        Some(path) => {
            let model_dir = Path::new(path);
            if !model_dir.exists() {
                return Err(RamariaError::validation(format!("模型目录不存在: {path}")));
            }

            let provider = ramaria_llm::embedding::native::create_native_provider_with_device(
                model_dir,
                engine.config().embedding.device,
            )?;
            let info = provider.model_info();
            tracing::info!(
                path = %path_log_label(model_dir),
                model = %info.model_id,
                dimension = info.dimension,
                "嵌入模型已加载，准备热更新"
            );

            let provider: Arc<dyn EmbeddingProvider> = Arc::new(provider);
            engine.update_embedding(Some(provider));
            config.embedding_model_path = Some(path.to_string());
        }
    }

    engine.storage_ref().save_backend_config(&config).await?;
    tracing::info!(
        has_model = config.embedding_model_path.is_some(),
        "嵌入模型配置已持久化"
    );
    Ok(())
}

/// 读取当前嵌入模型配置。
///
/// 返回:
/// - `Some(view)`: 模型已加载（含维度与可用性）或配置中留有路径（供 UI 预填）；
/// - `None`: 未加载且配置中无路径。
pub(crate) async fn embedding_model(engine: &Engine) -> RamariaResult<Option<EmbeddingModelView>> {
    if let Some(provider) = engine.embedding() {
        let info = provider.model_info();
        let view = EmbeddingModelView {
            // 模型已加载时不回传本地路径（只暴露维度与可用性）
            model_path: None,
            valid: provider.is_available(),
            dimension: Some(info.dimension),
        };
        tracing::debug!(
            dimension = info.dimension,
            valid = view.valid,
            "读取嵌入模型配置"
        );
        return Ok(Some(view));
    }

    let config = engine
        .storage_ref()
        .get_backend_config()
        .await?
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

/// 读取当前降级原因（仅在引擎处于 `Degraded` 状态时有值）。
///
/// 返回:
/// - `None`: 当前不处于降级状态；
/// - `Some(reason)`: 降级状态下的原因分类（LLM / 嵌入 / 两者）。
///
/// 说明:
/// - LLM 可用性以 `validate()` 实测（可能发起一次轻量网络探测）；
/// - 两者均可用但仍为 `Degraded`（本地 provider 以外的未知原因）返回 `Unknown`。
pub(crate) async fn degraded_reason(engine: &Engine) -> RamariaResult<Option<DegradedReason>> {
    if engine.current_state() != AppState::Degraded {
        return Ok(None);
    }

    let embedding_ok = engine.is_embedding_available();
    let llm_ok = engine.llm().validate().await.is_ok();

    let reason = match (llm_ok, embedding_ok) {
        (false, false) => DegradedReason::BothUnavailable,
        (false, true) => DegradedReason::LlmUnavailable,
        (true, false) => DegradedReason::EmbeddingMissing,
        (true, true) => DegradedReason::Unknown,
    };
    tracing::debug!(?reason, "当前降级原因");
    Ok(Some(reason))
}

// =========================================================
// 模型文件管理编排（列表 / 就绪 / 体积 / 删除 / 下载）
// =========================================================

/// 解析嵌入模型根目录（`None` = 平台默认目录）。
///
/// 参数:
/// - `override_path`: 宿主指定的模型根目录（设置页自定义路径）；`None` 时取默认。
///
/// 返回:
/// - 模型根目录路径（不保证已存在，由模型管理器按需创建）。
pub fn models_root(override_path: Option<&Path>) -> PathBuf {
    match override_path {
        Some(path) => path.to_path_buf(),
        None => ramaria_llm::model_manager::default_models_root(),
    }
}

/// 列出已安装的模型（三个必需文件齐全的模型目录名）。
pub fn list_models(root: Option<&Path>) -> RamariaResult<Vec<String>> {
    let manager = ModelManager::new(models_root(root))?;
    manager.list_installed_models()
}

/// 判定模型是否已就绪（config.json / model.safetensors / tokenizer.json 齐全）。
///
/// 说明:
/// - 管理器初始化失败（目录 / HTTP 客户端构造）按"未就绪"处理，不抛错。
pub fn is_model_ready(model_id: &str, root: Option<&Path>) -> bool {
    match ModelManager::new(models_root(root)) {
        Ok(manager) => manager.is_model_ready(model_id),
        Err(e) => {
            tracing::warn!(error = %e, "模型管理器初始化失败，判定为未就绪");
            false
        }
    }
}

/// 获取模型目录占用的磁盘空间（字节；管理器初始化失败返回 0）。
pub fn model_size(model_id: &str, root: Option<&Path>) -> u64 {
    match ModelManager::new(models_root(root)) {
        Ok(manager) => manager.model_size(model_id),
        Err(e) => {
            tracing::warn!(error = %e, "模型管理器初始化失败，体积按 0 返回");
            0
        }
    }
}

/// 模型删除结果。
///
/// 字段约定:
/// - `removed`: 本次是否实际删除（`false` = 目标模型本就不存在）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveModelOutcome {
    /// 是否实际删除（false = 本就不存在）
    pub removed: bool,
}

/// 删除指定模型的所有文件（幂等）。
///
/// 返回:
/// - `removed = true`: 目标目录存在且已删除；
/// - `removed = false`: 目标目录不存在（幂等成功，不报错）。
pub fn remove_model(model_id: &str, root: Option<&Path>) -> RamariaResult<RemoveModelOutcome> {
    let manager = ModelManager::new(models_root(root))?;
    let dir = manager.model_dir(model_id);
    if !dir.exists() {
        tracing::info!(model_id, "模型目录不存在，删除按幂等成功处理");
        return Ok(RemoveModelOutcome { removed: false });
    }
    manager.remove_model(model_id)?;
    Ok(RemoveModelOutcome { removed: true })
}

/// 下载嵌入模型（断点续传 + SHA-256 校验 + 原子替换，由模型管理器实现）。
///
/// 参数:
/// - `model_id`: 模型标识（须在预置清单内，否则发起网络请求前即失败）。
/// - `root`: 模型根目录覆盖（`None` 用平台默认）。
/// - `progress`: 可选下载进度回调。
pub async fn download_model(
    model_id: &str,
    root: Option<&Path>,
    progress: Option<ProgressCallback>,
) -> RamariaResult<()> {
    let manager = ModelManager::new(models_root(root))?;
    tracing::info!(model_id, "开始下载嵌入模型");
    manager.download_model(model_id, progress).await
}

// =========================================================
// 引擎门面
// =========================================================

impl Engine {
    /// 写入 LLM 后端配置（显式选项：keychain / 落库 / 热替换 / 文件同步）。
    ///
    /// 参数:
    /// - `config`: 新的后端配置（provider / base_url / model / 嵌入路径等）；
    /// - `options`: 写入选项（各步骤开关，见 [`BackendConfigWriteOptions`]）。
    ///
    /// 返回:
    /// - 各步骤完成情况与失败原因（见 [`BackendConfigWriteOutcome`]）；
    /// - 错误语义与用例实现一致：密钥 / 落库 / 热替换失败按分类返回，文件侧失败不阻塞。
    pub async fn write_backend_config(
        &self,
        config: &BackendConfig,
        options: BackendConfigWriteOptions,
    ) -> RamariaResult<BackendConfigWriteOutcome> {
        write_backend_config(self, config, &options).await
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
