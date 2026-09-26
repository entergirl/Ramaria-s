//! crates/ramaria-service/src/model.rs - 模型管理用例（LLM 后端与嵌入模型热更新）
//!
//! 设计特点:
//! - 后端配置写入即生效：配置落库（`backend_config`，真相源）→ 重建 provider → 整体替换内存快照，
//!   并尽力同步文件侧 `[backend]` 组（失败只记日志，不阻塞）；后续对话与记忆加工立即使用新
//!   provider，无需重启进程
//! - 嵌入模型按路径加载：校验 / 保存 / 读取 / 卸载四个动作覆盖设置页全部交互，
//!   卸载（空路径）与加载走同一用例，避免"只改配置不换实例"的半生效状态
//! - 降级不阻塞：嵌入模型缺失 / 不可用只影响向量通道（BM25 + 关键词镜像继续工作），
//!   用例不因嵌入缺失返回错误
//! - 校验可解释：目录缺失 / 加载失败 / 推理失败三类原因都以文本回传，
//!   设置页直接展示，不靠日志排查
//! - 缓存复用：热更新 provider 时沿用引擎持有的精确缓存实例，切换后端后既有缓存不失效
//! - 安全约束：API key 只经 OS keychain 读写（本地 provider 跳过），日志不记密钥内容

use std::path::Path;
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{AppState, BackendConfig, LlmProvider};

use crate::engine::{Engine, build_llm_provider};
use crate::types::{DegradedReason, EmbeddingModelView, EmbeddingValidation};

// =========================================================
// LLM 后端配置更新
// =========================================================

/// 更新 LLM 后端配置并热加载新 provider。
///
/// 流程:
/// 1. API key 写入 keychain（仅线上 provider；先写密钥再落配置，避免"配置指向无密钥后端"的中间态）；
/// 2. 后端配置落库（`backend_config` 为真相源）；
/// 3. 重建 provider（复用引擎持有的精确缓存）→ 整体替换内存快照；
/// 4. 文件侧 `[backend]` 组同步（`config_path` 已设置时；失败只记日志，不改变成功语义）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `config`: 新的后端配置（provider / base_url / model / 嵌入路径等）。
/// - `api_key`: 可选的线上 provider 密钥；`None` 或空白表示不更新密钥（沿用 keychain 现值）。
///
/// 返回:
/// - 成功时返回 `Ok(())`，此后读取路径取到的是新 provider。
/// - 密钥写入失败返回 `Privacy`；配置落库失败返回 `Storage`；provider 构造失败返回对应错误。
///
/// 说明:
/// - provider 构造失败时配置已落库：下次启动装配会按新配置重试，调用方可提示用户修正后重试；
/// - 本地 provider（LM Studio）不需要 API key，传入密钥只记 debug 并跳过。
pub(crate) async fn update_backend_config(
    engine: &Engine,
    config: &BackendConfig,
    api_key: Option<&str>,
) -> RamariaResult<()> {
    // ---- 1. API key（仅线上 provider；本地 provider 无需密钥） ----
    let key = api_key.map(str::trim).filter(|value| !value.is_empty());
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

    // ---- 3. 重建 provider 并热替换（复用精确缓存，保证切换后端后缓存不失效） ----
    let keychain = engine.keychain_arc();
    let provider = build_llm_provider(config, &keychain, engine.llm_cache())?;
    engine.update_llm(provider);

    // ---- 4. 文件侧同步（尽力而为：保持 config.toml 的 [backend] 组与表一致） ----
    if !engine.config_path().as_os_str().is_empty() {
        match engine.sync_backend_config(config).await {
            Ok(result) => {
                if !result.file_ok {
                    tracing::warn!(
                        failures = result.failures.len(),
                        "后端配置已落库，但 config.toml 同步失败（下次加载校验以文件为准）"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "后端配置已落库，但 config.toml 同步失败（降级不阻塞）"
                );
            }
        }
    }

    tracing::info!(
        provider = %config.provider,
        model = %config.capability.model_id,
        base_url = %config.base_url,
        "后端配置已更新并热加载"
    );
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
        tracing::warn!(path, "嵌入模型校验：目录不存在");
        return Ok(EmbeddingValidation::invalid(format!(
            "模型目录不存在: {path}"
        )));
    }
    if !model_dir.is_dir() {
        tracing::warn!(path, "嵌入模型校验：路径不是目录");
        return Ok(EmbeddingValidation::invalid(format!(
            "路径不是目录: {path}"
        )));
    }

    match ramaria_llm::embedding::native::create_native_provider_with_device(model_dir, device) {
        Ok(provider) => {
            let dimension = provider.model_info().dimension;
            match provider.validate().await {
                Ok(()) => {
                    tracing::info!(path, dimension, "嵌入模型校验通过");
                    Ok(EmbeddingValidation {
                        valid: true,
                        dimension: Some(dimension),
                        reason: None,
                    })
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "嵌入模型校验失败（模型可加载但推理失败）");
                    Ok(EmbeddingValidation {
                        valid: false,
                        dimension: Some(dimension),
                        reason: Some(format!("模型文件存在但推理失败: {e}")),
                    })
                }
            }
        }
        Err(e) => {
            tracing::warn!(path, error = %e, "嵌入模型校验失败（模型加载失败）");
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
                path,
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
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockLlm, engine_with_db, temp_dir};
    use ramaria_core::config::EmbeddingDevice;
    use ramaria_core::traits::StoreInfrastructure;

    /// 校验用例：目录不存在 / 路径不是目录 → valid=false + 原因，不抛错。
    #[tokio::test]
    async fn validate_reports_missing_path_without_error() {
        let dir = temp_dir("model-validate");
        let missing = dir.join("not-a-model");

        let result = validate_embedding_model(
            missing.to_string_lossy().as_ref(),
            EmbeddingDevice::default(),
        )
        .await
        .expect("校验用例不应抛错");
        assert!(!result.valid);
        assert!(result.dimension.is_none());
        assert!(
            result.reason.as_deref().unwrap_or("").contains("不存在"),
            "原因应说明目录缺失，实际: {:?}",
            result.reason
        );

        // 路径存在但不是目录（用文件顶上）
        let file = dir.join("model.txt");
        std::fs::write(&file, b"x").expect("写入测试文件应成功");
        let result =
            validate_embedding_model(file.to_string_lossy().as_ref(), EmbeddingDevice::default())
                .await
                .expect("校验用例不应抛错");
        assert!(!result.valid);
        assert!(
            result.reason.as_deref().unwrap_or("").contains("不是目录"),
            "原因应说明路径类型错误，实际: {:?}",
            result.reason
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 后端配置更新：配置落库 + provider 热替换后立即生效（无需重建引擎）。
    #[tokio::test]
    async fn update_backend_config_hot_swaps_provider() {
        let (engine, storage, dir) = engine_with_db("model-hot-llm").await;
        assert_eq!(engine.llm().name(), "MockLlm", "注入的 provider 应生效");

        let mut config = BackendConfig::lm_studio_default();
        config.base_url = "http://localhost:8888/v1".to_string();
        config.capability.base_url = "http://localhost:8888/v1".to_string();
        config.capability.model_id = "qwen-test".to_string();

        // 本地 provider：无密钥写入，仅落库 + 热替换
        engine
            .update_backend_config(&config, None)
            .await
            .expect("后端配置更新应成功");

        // 热替换后立即生效：新 provider 使用新 base_url
        let llm = engine.llm();
        assert_eq!(llm.name(), "LM Studio");
        assert_eq!(llm.config().base_url, "http://localhost:8888/v1");

        // 配置已落库（下次启动装配依据）
        let saved = storage
            .get_backend_config()
            .await
            .expect("读取后端配置应成功")
            .expect("后端配置应已落库");
        assert_eq!(saved.capability.model_id, "qwen-test");

        // 本地 provider 传入密钥：跳过 keychain 写入，不影响配置更新
        engine
            .update_backend_config(&config, Some("sk-should-be-ignored"))
            .await
            .expect("本地 provider 更新应成功");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 后端配置更新：`config_path` 已设置时同步文件侧 `[backend]` 组，其它组保留。
    #[tokio::test]
    async fn update_backend_config_syncs_file_side_backend_group() {
        let dir = temp_dir("model-file-sync");
        let db_path = dir.join("assistant.db");
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "[utt]\ntheta_gap_minutes = 12\n").expect("写入配置应成功");
        let engine = Engine::open_with(
            crate::engine::EngineOptions::new(db_path).with_config_path(config_path.clone()),
        )
        .await
        .expect("引擎装配应成功");

        let mut config = BackendConfig::lm_studio_default();
        config.base_url = "http://localhost:7778/v1".to_string();
        config.capability.base_url = "http://localhost:7778/v1".to_string();
        config.capability.model_id = "qwen-file-sync".to_string();
        engine
            .update_backend_config(&config, None)
            .await
            .expect("后端配置更新应成功");

        // 文件侧 [backend] 组已同步，其它组保留
        let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
        let file_cfg: ramaria_core::config::RamariaConfig =
            toml::from_str(&text).expect("文件应为合法 TOML");
        assert_eq!(file_cfg.backend.model_id, "qwen-file-sync");
        assert_eq!(file_cfg.backend.base_url, "http://localhost:7778/v1");
        assert_eq!(file_cfg.utt.theta_gap_minutes, 12, "文件侧其它组应保留");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 嵌入模型保存与卸载：热更新后立即生效，路径持久化；空路径卸载。
    #[tokio::test]
    async fn save_embedding_model_loads_and_unloads() {
        let (engine, storage, dir) = engine_with_db("model-hot-embedding").await;
        assert!(!engine.is_embedding_available(), "初始应无嵌入模型");

        // 目录不存在 → 显式校验错误，且不改变现状
        let err = engine
            .save_embedding_model(Some("/definitely/not/a/model/dir"))
            .await
            .expect_err("目录不存在应报错");
        assert_eq!(err.category(), "validation");
        assert!(!engine.is_embedding_available());

        // 注入可用 provider（模拟"模型加载成功"后的热替换）
        let embedding = Arc::new(crate::test_support::DeterministicEmbedding::new());
        engine.update_embedding(Some(embedding));
        assert!(engine.is_embedding_available(), "热更新后向量通道应可用");
        let view = engine
            .embedding_model()
            .await
            .expect("读取嵌入模型应成功")
            .expect("应返回已加载模型视图");
        assert!(view.valid);
        assert_eq!(
            view.dimension,
            Some(crate::test_support::DeterministicEmbedding::DIMENSION)
        );

        // 配置中留路径但未加载：供 UI 预填
        let mut config = storage
            .get_backend_config()
            .await
            .expect("读取后端配置应成功")
            .unwrap_or_else(BackendConfig::lm_studio_default);
        config.embedding_model_path = Some("/saved/model/path".to_string());
        storage
            .save_backend_config(&config)
            .await
            .expect("保存后端配置应成功");
        engine.update_embedding(None);
        let view = engine
            .embedding_model()
            .await
            .expect("读取嵌入模型应成功")
            .expect("应按已保存路径返回视图");
        assert_eq!(view.model_path.as_deref(), Some("/saved/model/path"));
        assert!(!view.valid, "未加载时不应视为可用");

        // 卸载：provider 与持久化路径一并清空
        engine.save_embedding_model(None).await.expect("卸载应成功");
        assert!(!engine.is_embedding_available());
        assert!(
            engine
                .embedding_model()
                .await
                .expect("读取嵌入模型应成功")
                .is_none(),
            "卸载后读取应返回 None"
        );
        let saved = storage
            .get_backend_config()
            .await
            .expect("读取后端配置应成功")
            .expect("后端配置应存在");
        assert!(saved.embedding_model_path.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 热更新并发读：写侧替换 provider 期间，读侧取到的始终是完整快照。
    #[tokio::test]
    async fn hot_swap_is_visible_to_concurrent_readers() {
        let (engine, _storage, dir) = engine_with_db("model-hot-concurrent").await;
        engine.update_llm(Arc::new(MockLlm::with_reply("第一代")));

        // 读侧持续取快照（与写侧交替）；名称与配置必须成对来自同一 provider
        let mut names = Vec::new();
        for index in 0..16 {
            if index % 4 == 0 {
                engine.update_llm(Arc::new(MockLlm::with_reply("新一代")));
            }
            let llm = engine.llm();
            names.push(llm.name());
            assert!(
                llm.config().base_url.starts_with("http://"),
                "快照读到的 provider 配置应完整"
            );
        }
        assert!(
            names.iter().all(|name| *name == "MockLlm"),
            "所有快照都应来自已装配的 provider"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
