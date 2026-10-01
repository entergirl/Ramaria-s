//! crates/ramaria-service/src/setup.rs - 首次配置流程用例（状态机推进与缺项诊断）
//!
//! 设计特点:
//! - 一次调用完成首次配置：密钥入 keychain → 后端配置落库 → provider 热替换 → 健康探测 →
//!   状态机推进，调用方（桌面向导 / CLI 向导）无需自行装配 provider
//! - 幂等：重跑向导不丢已保存的嵌入模型路径（向量通道配置与 LLM 后端配置分属两件事）；
//!   重复提交同一后端配置只产生一次等效结果
//! - 降级不阻塞：健康探测失败只把状态置为 `Degraded`（BM25 + 关键词镜像仍可用），
//!   不返回错误，用户可修正配置后重试
//! - 诊断可读：缺项清单（`SetupStatus::missing_items`）覆盖后端配置 / 模型选择 / 索引 / 嵌入
//!   四项，设置页直接展示
//! - 状态口径单一：`NeedsSetup → Indexing → Ready / Degraded` 的判定集中在 `determine_state`，
//!   读状态与刷新状态共用同一实现
//! - 安全约束：API key 只经 OS keychain 落盘，日志不记密钥内容

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::LlmProvider as LlmProviderTrait;
use ramaria_core::types::{AppState, BackendConfig};

use crate::engine::Engine;
use crate::types::{SetupRequest, SetupStatus};

// =========================================================
// 探测参数
// =========================================================

/// 健康探测最大尝试次数（与既有向导口径一致）。
pub const HEALTH_PROBE_ATTEMPTS: u32 = 3;

/// 健康探测重试间隔（秒）。
pub const HEALTH_PROBE_INTERVAL_SECONDS: u64 = 2;

// =========================================================
// 缺项诊断与状态判定
// =========================================================

/// 读取设置检查结果（嵌入可用性取引擎当前快照）。
///
/// 返回:
/// - 后端配置 / 模型选择 / 索引状态 / 嵌入可用性四项判定。
pub(crate) async fn check(engine: &Engine) -> RamariaResult<SetupStatus> {
    let embedding_available = engine.is_embedding_available();
    check_with(engine, embedding_available).await
}

/// 按指定嵌入可用性读取设置检查结果。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `embedding_available`: 嵌入可用性（读状态取引擎快照）。
///
/// 检查项:
/// 1. 后端配置：`backend_config` 是否有记录；
/// 2. 模型选择：线上 provider 要求 `capability.model_id` 非空，本地 provider 视为已选；
/// 3. 索引状态：`schema_meta.index_version == 0` 表示尚未构建
///    （该键缺失时同样按未构建口径返回 `0`）。
async fn check_with(engine: &Engine, embedding_available: bool) -> RamariaResult<SetupStatus> {
    let storage = engine.storage_ref().as_ref();
    let backend_config = storage.get_backend_config().await?;

    let backend_configured = backend_config.is_some();
    let model_selected = backend_config
        .as_ref()
        .map(|config| {
            if config.provider.is_online() {
                !config.capability.model_id.is_empty()
            } else {
                // 本地 provider：模型由本地推理服务侧决定，配置层不强制
                true
            }
        })
        .unwrap_or(false);

    let index_version = storage.get_index_version().await?;
    let needs_indexing = index_version == 0;

    tracing::debug!(
        backend_configured,
        model_selected,
        index_version,
        needs_indexing,
        embedding_available,
        "设置状态检查完成"
    );

    Ok(SetupStatus {
        backend_configured,
        model_selected,
        needs_indexing,
        embedding_available,
    })
}

/// 根据设置状态判定应用状态。
///
/// 返回:
/// - `NeedsSetup`: 后端配置或模型选择未完成；
/// - `Indexing`: 索引待构建；
/// - `Degraded`: 核心配置就绪但嵌入模型不可用（向量通道降级）；
/// - `Ready`: 核心配置就绪且嵌入模型可用。
pub(crate) fn determine_state(status: &SetupStatus) -> AppState {
    if !status.backend_configured || !status.model_selected {
        AppState::NeedsSetup
    } else if status.needs_indexing {
        AppState::Indexing
    } else if !status.embedding_available {
        AppState::Degraded
    } else {
        AppState::Ready
    }
}

// =========================================================
// 配置流程
// =========================================================

/// 执行首次配置：写入后端配置与密钥 → 热替换 provider → 健康探测 → 推进状态机。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 向导提交的后端选择（provider / model / base_url / api_key）。
///
/// 返回:
/// - 探测通过时返回按缺项诊断判定的状态：索引待构建为 `Indexing`，索引已构建且
///   嵌入可用为 `Ready`，嵌入不可用为 `Degraded`；
/// - 探测全部失败时返回 `Degraded`（不报错，用户可修正配置后重试）；
/// - 线上 provider 缺少 API key 返回 `Validation`（先于任何写入返回）。
///
/// 说明:
/// - 已保存的嵌入模型路径在本次配置中保留（重跑向导不丢向量通道配置）；
/// - 索引构建与嵌入模型加载不在本用例内触发，由索引用例与模型用例负责。
pub(crate) async fn run(engine: &Engine, req: &SetupRequest) -> RamariaResult<AppState> {
    apply(engine, req).await?;

    // ---- 健康探测（失败只降级，不阻塞后续操作） ----
    let llm = engine.llm_ref();
    let health_ok = probe_health_with_retry(
        llm.as_ref(),
        HEALTH_PROBE_ATTEMPTS,
        HEALTH_PROBE_INTERVAL_SECONDS,
    )
    .await;

    advance_state(engine, health_ok).await
}

/// 写入配置并热替换 provider（不含健康探测）。
///
/// 返回:
/// - 本次生效的后端配置（调用方可用于展示或后续处理）。
///
/// 说明:
/// - 与 [`run`] 拆分是为了让"配置写入"与"健康探测"可分别验证：探测依赖真实后端，
///   配置写入不依赖；
/// - 线上 provider 缺少 API key 时返回 `Validation`，且不产生任何写入。
pub(crate) async fn apply(engine: &Engine, req: &SetupRequest) -> RamariaResult<BackendConfig> {
    // ---- 线上 provider 必须有 API key（缺失属参数错误，先于任何写入返回） ----
    if req.provider.is_online() && req.api_key.as_deref().unwrap_or("").trim().is_empty() {
        tracing::warn!(provider = %req.provider, "首次配置缺少 API key，未写入任何配置");
        return Err(RamariaError::validation(format!(
            "{} 需要 API key，请填写后重试",
            req.provider
        )));
    }

    // ---- 后端配置：保留已保存的嵌入模型路径 ----
    let existing_embedding_path = engine
        .storage_ref()
        .get_backend_config()
        .await?
        .and_then(|config| config.embedding_model_path);

    let mut config =
        BackendConfig::new_with_defaults(req.provider, req.base_url.clone(), req.model_id.clone());
    config.embedding_model_path = existing_embedding_path;

    // ---- 密钥入 keychain + 配置落库 + provider 热替换（模型管理用例，单份实现） ----
    crate::model::update_backend_config(engine, &config, req.api_key.as_deref()).await?;
    Ok(config)
}

/// 按健康探测结果推进应用状态。
///
/// 参数:
/// - `health_ok`: 后端健康探测结果。
///
/// 返回:
/// - 探测失败 → `Degraded`（配置已落库，用户可修正后重试）；
/// - 探测通过 → 按缺项诊断判定（配置完整度 + 索引状态 + 嵌入可用性；
///   三者齐备时本步骤直接判定为 `Ready`，嵌入缺失时为 `Degraded`）。
pub(crate) async fn advance_state(engine: &Engine, health_ok: bool) -> RamariaResult<AppState> {
    let state = if health_ok {
        tracing::info!(provider = %engine.llm().name(), "LLM 后端健康检查通过");
        determine_state(&check(engine).await?)
    } else {
        tracing::warn!(
            provider = %engine.llm().name(),
            "LLM 后端健康检查失败，应用进入 Degraded 状态"
        );
        AppState::Degraded
    };
    engine.set_state(state);
    Ok(state)
}

/// 刷新应用状态（从存储重读配置并重新判定）。
///
/// 返回:
/// - 按当前后端配置 / 索引状态 / 嵌入可用性判定的应用状态。
///
/// 用法:
/// - 索引构建完成、嵌入模型热加载、配置文件变更后调用，使状态与事实一致。
pub(crate) async fn refresh(engine: &Engine) -> RamariaResult<AppState> {
    let status = check(engine).await?;
    let state = determine_state(&status);
    engine.set_state(state);
    Ok(state)
}

/// 带重试的 LLM 后端健康探测。
///
/// 参数:
/// - `llm`: 待探测的 provider。
/// - `max_retries`: 最大尝试次数（含首次；0 按 1 次处理）。
/// - `interval_secs`: 相邻尝试之间的间隔秒数。
///
/// 返回:
/// - `true`: 至少一次探测通过；
/// - `false`: 全部尝试失败。
///
/// 说明:
/// - 探测使用 `health_check`（轻量：只验证后端可达），不做完整 `validate`；
/// - 重试用于吸收本地推理服务启动中的短暂不可达（如 LM Studio 正在加载模型）。
pub(crate) async fn probe_health_with_retry(
    llm: &dyn LlmProviderTrait,
    max_retries: u32,
    interval_secs: u64,
) -> bool {
    let attempts = max_retries.max(1);
    for attempt in 0..attempts {
        match llm.health_check().await {
            Ok(()) => return true,
            Err(e) => {
                if attempt + 1 < attempts {
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_retries = attempts,
                        error = %e,
                        "健康检查失败，即将重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
                } else {
                    tracing::error!(
                        attempt = attempt + 1,
                        max_retries = attempts,
                        error = %e,
                        "健康检查全部失败"
                    );
                }
            }
        }
    }
    false
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        DeterministicEmbedding, MockLlm, engine_with_db, engine_with_llm_and_config,
        engine_with_llm_config_and_embedding,
    };
    use ramaria_core::traits::{EmbeddingProvider, StoreInfrastructure};
    use ramaria_core::types::LlmProvider as LlmProviderKind;
    use std::sync::Arc;

    /// 构造首次配置请求（本地 provider：无需 API key）。
    fn local_request(base_url: &str) -> SetupRequest {
        SetupRequest {
            provider: LlmProviderKind::LmStudio,
            model_id: "local-model".to_string(),
            base_url: base_url.to_string(),
            api_key: None,
        }
    }

    /// 启动本地 mock HTTP 服务：对任意请求返回 200（供健康探测通过）。
    ///
    /// 说明:
    /// - 返回 mock 服务 base_url；循环接受连接以吸收探测重试；
    /// - 服务任务随测试 runtime 结束而终止。
    async fn spawn_mock_health_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定 mock 端口应成功");
        let addr = listener.local_addr().expect("获取 mock 地址应成功");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                // 读到请求头结束即可（GET 无 body）
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                while buf.len() < 8192 {
                    match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://127.0.0.1:{}/v1", addr.port())
    }

    /// 缺项诊断：空库缺后端 / 模型 / 索引 / 嵌入四项；显式置索引版本为已构建后去掉索引项；
    /// 本地 / 线上 provider 的模型选择口径不同。
    #[tokio::test]
    async fn check_reports_missing_items_on_empty_db() {
        let (engine, storage, dir) = engine_with_db("setup-empty").await;

        // 空库 migration 后索引版本为 0（未构建）：缺后端 / 模型 / 索引 / 嵌入四项
        let status = engine.check_setup_status().await.expect("诊断应成功");
        assert!(!status.backend_configured);
        assert!(!status.model_selected);
        assert!(
            status.needs_indexing,
            "新库迁移后索引版本为 0，空库按未构建口径"
        );
        assert!(!status.embedding_available);
        assert!(!status.is_complete());
        assert_eq!(
            status.missing_items().len(),
            4,
            "缺后端 / 模型 / 索引 / 嵌入"
        );
        assert!(
            status
                .missing_items()
                .iter()
                .any(|item| item.contains("索引")),
            "缺项应包含索引项"
        );

        // 显式标记索引已构建：缺项清单去掉索引项
        storage
            .set_index_version(1)
            .await
            .expect("写入索引版本应成功");
        let status = engine.check_setup_status().await.expect("诊断应成功");
        assert!(!status.needs_indexing);
        assert_eq!(status.missing_items().len(), 3, "缺后端 / 模型 / 嵌入");

        // 本地 provider：模型选择视为完成
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");
        let status = engine.check_setup_status().await.expect("诊断应成功");
        assert!(status.backend_configured);
        assert!(status.model_selected, "本地 provider 不强制 model_id");

        // 线上 provider 且 model_id 为空：模型选择未完成
        let online = BackendConfig::new_with_defaults(
            LlmProviderKind::DeepSeek,
            "https://api.deepseek.com/v1".to_string(),
            String::new(),
        );
        storage
            .save_backend_config(&online)
            .await
            .expect("保存后端配置应成功");
        let status = engine.check_setup_status().await.expect("诊断应成功");
        assert!(
            !status.model_selected,
            "线上 provider 空 model_id 应视为未选模型"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状态判定：各组合逐一对应 NeedsSetup / Indexing / Degraded / Ready。
    #[test]
    fn determine_state_cases() {
        let cases = [
            (
                SetupStatus {
                    backend_configured: false,
                    model_selected: false,
                    needs_indexing: false,
                    embedding_available: false,
                },
                AppState::NeedsSetup,
            ),
            (
                SetupStatus {
                    backend_configured: true,
                    model_selected: false,
                    needs_indexing: false,
                    embedding_available: true,
                },
                AppState::NeedsSetup,
            ),
            (
                SetupStatus {
                    backend_configured: true,
                    model_selected: true,
                    needs_indexing: true,
                    embedding_available: true,
                },
                AppState::Indexing,
            ),
            (
                SetupStatus {
                    backend_configured: true,
                    model_selected: true,
                    needs_indexing: false,
                    embedding_available: false,
                },
                AppState::Degraded,
            ),
            (
                SetupStatus {
                    backend_configured: true,
                    model_selected: true,
                    needs_indexing: false,
                    embedding_available: true,
                },
                AppState::Ready,
            ),
        ];
        for (status, expected) in cases {
            assert_eq!(
                determine_state(&status),
                expected,
                "状态判定不符: {status:?}"
            );
        }
    }

    /// 配置写入：落库 + provider 热替换 + 保留嵌入路径（重跑幂等），不依赖网络。
    #[tokio::test]
    async fn apply_persists_config_and_preserves_embedding_path() {
        let (engine, storage, dir) = engine_with_db("setup-apply").await;

        // 预置嵌入模型路径：模拟"上一次已配置嵌入模型"
        let mut seeded = BackendConfig::lm_studio_default();
        seeded.embedding_model_path = Some("/saved/embedding/model".to_string());
        storage
            .save_backend_config(&seeded)
            .await
            .expect("保存后端配置应成功");

        let request = local_request("http://localhost:7777/v1");
        let applied = apply(&engine, &request).await.expect("配置写入应成功");
        assert_eq!(applied.base_url, "http://localhost:7777/v1");
        assert_eq!(applied.capability.model_id, "local-model");

        // 配置已落库且嵌入路径保留
        let saved = storage
            .get_backend_config()
            .await
            .expect("读取配置应成功")
            .expect("配置应存在");
        assert_eq!(saved.base_url, "http://localhost:7777/v1");
        assert_eq!(
            saved.embedding_model_path.as_deref(),
            Some("/saved/embedding/model"),
            "重跑向导不得清空已保存的嵌入模型路径"
        );

        // provider 已热替换（新 provider 使用新 base_url）
        assert_eq!(engine.llm().name(), "LM Studio");
        assert_eq!(engine.llm().config().base_url, "http://localhost:7777/v1");

        // 幂等：再次提交同一配置，结果一致
        let again = apply(&engine, &request).await.expect("重复配置应成功");
        assert_eq!(again.base_url, applied.base_url);
        assert_eq!(again.capability.model_id, applied.capability.model_id);
        assert_eq!(again.embedding_model_path, applied.embedding_model_path);
        let saved_again = storage
            .get_backend_config()
            .await
            .expect("读取配置应成功")
            .expect("配置应存在");
        assert_eq!(
            saved_again.embedding_model_path.as_deref(),
            Some("/saved/embedding/model")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状态推进：探测失败一律 Degraded；探测通过按缺项诊断判定（含嵌入可用性）。
    #[tokio::test]
    async fn advance_state_follows_probe_and_diagnostics() {
        let (engine, storage, dir) = engine_with_db("setup-advance").await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");

        // 探测失败：配置完整性无关，直接降级
        assert_eq!(
            advance_state(&engine, false).await.expect("推进应成功"),
            AppState::Degraded
        );
        assert_eq!(engine.current_state(), AppState::Degraded);

        // 探测通过 + 索引待构建 → Indexing
        storage
            .set_index_version(0)
            .await
            .expect("写入索引版本应成功");
        assert_eq!(
            advance_state(&engine, true).await.expect("推进应成功"),
            AppState::Indexing
        );

        // 探测通过 + 索引已构建 + 嵌入不可用 → Degraded
        storage
            .set_index_version(1)
            .await
            .expect("写入索引版本应成功");
        assert_eq!(
            advance_state(&engine, true).await.expect("推进应成功"),
            AppState::Degraded
        );

        // 嵌入可用后：推进路径与刷新路径均判定 Ready
        let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
        engine.update_embedding(Some(embedding));
        assert_eq!(
            advance_state(&engine, true).await.expect("推进应成功"),
            AppState::Ready
        );
        assert_eq!(
            engine.refresh_setup_state().await.expect("刷新应成功"),
            AppState::Ready
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 探测失败：配置写入成功后探测失败 → 降级为 Degraded，配置保留（用户可修正后重试）。
    ///
    /// 说明:
    /// - "探测失败"以 `advance_state(false)` 作为输入直接构造：探测重试口径由
    ///   `probe_health_retries_until_success` 覆盖，本用例聚焦失败时的降级与配置保留；
    /// - 配置写入走真实路径（`apply`：真实 provider 构建与热替换），地址不参与探测。
    #[tokio::test]
    async fn run_degrades_when_probe_fails() {
        let (engine, storage, dir) = engine_with_db("setup-degraded").await;

        // 配置写入（真实 provider 构建与热替换）
        apply(&engine, &local_request("http://127.0.0.1:9/v1"))
            .await
            .expect("配置写入应成功");

        // 探测失败输入 → 状态推进为 Degraded（不报错）
        let state = advance_state(&engine, false)
            .await
            .expect("探测失败也应成功返回（降级而非报错）");
        assert_eq!(state, AppState::Degraded);
        assert_eq!(engine.current_state(), AppState::Degraded);
        assert!(
            storage
                .get_backend_config()
                .await
                .expect("读取配置应成功")
                .is_some(),
            "探测失败不影响配置落库（用户可修正后重试）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 首次配置：配置就绪 + 索引已构建 + 嵌入可用 → 直接返回 Ready（无需二次刷新推进）。
    ///
    /// 健康探测对象为配置写入后重建的真实 provider，故用本地 mock 服务让探测通过，
    /// 聚焦状态判定本身。
    #[tokio::test]
    async fn run_setup_reaches_ready_when_embedding_available() {
        let health_url = spawn_mock_health_server().await;
        let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
        let (engine, storage, dir) = engine_with_llm_config_and_embedding(
            "setup-ready",
            MockLlm::local(),
            ramaria_core::config::RamariaConfig::default(),
            Some(embedding),
        )
        .await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");
        storage
            .set_index_version(1)
            .await
            .expect("写入索引版本应成功");

        let state = engine
            .run_setup(&local_request(&health_url))
            .await
            .expect("首次配置应成功");
        assert_eq!(
            state,
            AppState::Ready,
            "配置就绪 + 索引已建 + 嵌入可用时应直接判定 Ready"
        );
        assert_eq!(engine.current_state(), AppState::Ready);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 首次配置：配置就绪 + 索引已构建但嵌入缺失 → Degraded（向量通道降级，不阻塞）。
    ///
    /// 探测经本地 mock 服务通过，确保降级原因只来自嵌入缺失而非探测失败。
    #[tokio::test]
    async fn run_setup_stays_degraded_without_embedding() {
        let health_url = spawn_mock_health_server().await;
        let (engine, storage, dir) = engine_with_llm_and_config(
            "setup-no-embedding",
            MockLlm::local(),
            ramaria_core::config::RamariaConfig::default(),
        )
        .await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");
        storage
            .set_index_version(1)
            .await
            .expect("写入索引版本应成功");

        let state = engine
            .run_setup(&local_request(&health_url))
            .await
            .expect("首次配置应成功");
        assert_eq!(state, AppState::Degraded, "嵌入缺失时首次配置应降级");
        assert_eq!(engine.current_state(), AppState::Degraded);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 线上 provider 缺少 API key：显式校验错误，且不写入任何配置。
    #[tokio::test]
    async fn run_setup_rejects_online_provider_without_key() {
        let (engine, storage, dir) = engine_with_db("setup-no-key").await;

        let err = engine
            .run_setup(&SetupRequest {
                provider: LlmProviderKind::DeepSeek,
                model_id: "deepseek-chat".to_string(),
                base_url: "https://api.deepseek.com/v1".to_string(),
                api_key: None,
            })
            .await
            .expect_err("缺少 API key 应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            storage
                .get_backend_config()
                .await
                .expect("读取配置应成功")
                .is_none(),
            "校验失败不得写入后端配置"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 刷新状态：索引版本推进与嵌入可用性共同决定 Ready / Degraded。
    #[tokio::test]
    async fn refresh_state_follows_index_and_embedding() {
        let (engine, storage, dir) = engine_with_db("setup-refresh").await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");

        // 索引待构建 → Indexing
        storage
            .set_index_version(0)
            .await
            .expect("写入索引版本应成功");
        assert_eq!(
            engine.refresh_setup_state().await.expect("刷新应成功"),
            AppState::Indexing
        );

        // 索引已构建 + 嵌入不可用 → Degraded
        storage
            .set_index_version(1)
            .await
            .expect("写入索引版本应成功");
        assert_eq!(
            engine.refresh_setup_state().await.expect("刷新应成功"),
            AppState::Degraded
        );
        assert_eq!(engine.current_state(), AppState::Degraded);

        // 降级原因：LLM 可用（mock 探测通过）、嵌入缺失
        assert_eq!(
            engine.degraded_reason().await.expect("读取降级原因应成功"),
            Some(crate::types::DegradedReason::EmbeddingMissing)
        );

        // 索引已构建 + 嵌入可用 → Ready
        let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
        engine.update_embedding(Some(embedding));
        assert_eq!(
            engine.refresh_setup_state().await.expect("刷新应成功"),
            AppState::Ready
        );
        assert_eq!(
            engine.degraded_reason().await.expect("读取降级原因应成功"),
            None,
            "非降级状态不返回降级原因"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 健康探测重试：前几次失败后成功 → true；超出上限 → false。
    #[tokio::test]
    async fn probe_health_retries_until_success() {
        // 前 2 次失败、第 3 次成功（探测间隔 0 秒，避免测试等待）
        let llm = MockLlm::local().with_health_failures(2);
        assert!(
            probe_health_with_retry(&llm, 3, 0).await,
            "重试后成功应返回 true"
        );

        // 失败次数超过上限 → false
        let llm = MockLlm::local().with_health_failures(5);
        assert!(
            !probe_health_with_retry(&llm, 2, 0).await,
            "尝试次数用尽应返回 false"
        );

        // 尝试次数为 0：按 1 次处理（不出现零次探测）
        let llm = MockLlm::local();
        assert!(probe_health_with_retry(&llm, 0, 0).await);
    }

    /// 缺索引版本的空库：判定未构建 → 一次重建写回版本（幂等）→ 刷新后推进到 Ready。
    ///
    /// 说明:
    /// - 构造"缺键"库（删除 migration 预置的索引版本键），模拟从未写过索引版本的老库；
    /// - 重建连续执行两次验证幂等；收敛后按嵌入可用性判定状态（本用例嵌入可用 → Ready）。
    #[tokio::test]
    async fn missing_index_version_converges_after_rebuild() {
        let embedding: Arc<dyn EmbeddingProvider> = Arc::new(DeterministicEmbedding::new());
        let (engine, storage, dir) = engine_with_llm_config_and_embedding(
            "setup-missing-version",
            MockLlm::local(),
            ramaria_core::config::RamariaConfig::default(),
            Some(embedding),
        )
        .await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");

        // 删除 migration 预置的索引版本键：构造"缺键"库
        let pool = engine.sqlite_pool().expect("测试库应附着连接池");
        sqlx::query("DELETE FROM schema_meta WHERE key = 'index_version'")
            .execute(&pool)
            .await
            .expect("删除索引版本键应成功");

        // 缺键 → 判定未构建 → Indexing
        let status = engine.check_setup_status().await.expect("诊断应成功");
        assert!(status.needs_indexing, "缺键应判定为未构建");
        assert_eq!(
            engine.refresh_setup_state().await.expect("刷新应成功"),
            AppState::Indexing
        );

        // 一次重建（幂等：连续两次）→ 版本写回 1 → 状态推进到 Ready
        engine.rebuild_index().await.expect("重建应成功");
        engine.rebuild_index().await.expect("重复重建应成功");
        assert_eq!(
            storage
                .get_index_version()
                .await
                .expect("读取索引版本应成功"),
            1,
            "重建完成后应写回索引版本 1"
        );
        assert_eq!(
            engine.refresh_setup_state().await.expect("刷新应成功"),
            AppState::Ready,
            "重建 + 刷新后应推进到 Ready"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
