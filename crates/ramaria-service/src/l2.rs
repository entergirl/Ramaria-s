//! crates/ramaria-service/src/l2.rs - L2 事件提取触发用例
//!
//! 设计特点:
//! - 单 persona 触发：该 persona 未吸收 L1 达到 `[thresholds].l2_trigger_count` 时执行一次提取
//! - 算法复用：提取本身走 `ramaria-memory` 的 `EventExtractor`（不在本层重写第二套）
//! - 阈值一致：提取器内部二次确认使用同一阈值，避免自定义阈值下静默跳过
//! - 降级纪律：读取失败 / LLM 不可用 / 提取失败由调用方记 warn 降级，不阻塞封存与回流
//! - 边界：桌面宿主的完整链路（L2→L3 级联、知识事实抽取、后台定时补扫）仍在 app；
//!   本用例覆盖 MCP 等无 app 宿主运行时"事件层不停滞"的最小触发
//! - 隐私：日志只记条数与 persona，不记事件内容

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_memory::event::{DegradeConfig, EventExtractor, EventExtractorConfig};

/// 检查并执行单 persona 的 L2 事件提取。
///
/// 流程:
/// 1. 读取该 persona 未吸收 L1；不足阈值 → 不触发（记 debug，直接返回 0）；
/// 2. 按配置组装 `EventExtractorConfig`
///    （LLM 参数 / 聚类去重指纹 / 降级置信度公式 / 请求间隔）；
/// 3. 执行提取并返回事件条数。
///
/// 参数:
/// - `storage`: 存储后端（L1 / 事件读写）。
/// - `llm`: LLM provider（不可用时调用失败，由调用方降级）。
/// - `config`: 生效配置（阈值与提取参数来源）。
/// - `persona_uid`: 目标人格。
///
/// 返回:
/// - `Ok(n)`: 本次提取到的事件条数（未触发 / 无产出为 0）。
/// - `Err(..)`: 读取未吸收 L1 失败或提取失败；调用方应记 warn 并继续封存主流程。
pub(crate) async fn check_and_extract(
    storage: &dyn StorageBackend,
    llm: &dyn LlmProvider,
    config: &RamariaConfig,
    persona_uid: &str,
) -> RamariaResult<usize> {
    // ---- 1. 触发条件：未吸收 L1 计数（与在线管线同一阈值） ----
    let unabsorbed = storage.list_unabsorbed_l1(persona_uid).await?;
    let trigger_count = config.thresholds.l2_trigger_count as usize;
    if unabsorbed.len() < trigger_count {
        tracing::debug!(
            persona_uid,
            unabsorbed_count = unabsorbed.len(),
            trigger_count,
            "L2 触发条件未满足，跳过事件提取"
        );
        return Ok(0);
    }
    tracing::info!(
        persona_uid,
        unabsorbed_count = unabsorbed.len(),
        trigger_count,
        "L2 触发条件满足，执行事件提取"
    );

    // ---- 2. 提取参数（字段口径与在线管线一致） ----
    let extractor_config = EventExtractorConfig {
        cluster_delay_ms: config.thresholds.cluster_delay_ms,
        temperature: config.event_extraction.temperature,
        max_tokens: config.event_extraction.max_tokens,
        max_events: config.event_extraction.max_events,
        trigger_count: config.thresholds.l2_trigger_count as i64,
        trigger_days: config.thresholds.l2_trigger_days as i64,
        l2_fingerprint_enabled: config.cache.l2_fingerprint_enabled,
        l2_similarity_threshold: config.cache.l2_similarity_threshold,
        l2_recent_events_limit: config.cache.l2_recent_events_limit,
        degrade: DegradeConfig {
            dynamic_confidence_enabled: config.event_extraction.degraded_confidence_enabled,
            ..Default::default()
        },
        ..Default::default()
    };

    // ---- 3. 执行提取（单次尝试；重试与可观测性由宿主调度链承担） ----
    let mut extractor = EventExtractor::new(llm, storage, extractor_config);
    let events = extractor.extract_events(persona_uid).await?;
    tracing::info!(persona_uid, event_count = events.len(), "L2 事件提取完成");
    Ok(events.len())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockLlm, engine_with_llm_and_config, seed_l1, seed_persona};

    /// 未达阈值：不触发提取、不调用 LLM，返回 0。
    #[tokio::test]
    async fn below_threshold_skips_extraction() {
        let (engine, storage, dir) =
            engine_with_llm_and_config("l2-skip", MockLlm::local(), RamariaConfig::default()).await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户提到最近在准备考试",
            Some("考试"),
            1_000,
        )
        .await;

        let count = check_and_extract(
            engine.storage_ref().as_ref(),
            engine.llm_ref().as_ref(),
            &engine.config(),
            "char-0001",
        )
        .await
        .expect("触发检查应成功");
        assert_eq!(count, 0, "未达阈值不应产出事件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 达阈值：触发提取并按提取器口径返回条数（LLM 由 mock 承担，无网络）。
    #[tokio::test]
    async fn reached_threshold_runs_extraction() {
        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_count = 1;
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;

        let (engine, storage, dir) =
            engine_with_llm_and_config("l2-run", MockLlm::with_reply(r#"{"events": []}"#), config)
                .await;
        seed_persona(&storage, "char-0001").await;
        seed_l1(
            &storage,
            "char-0001",
            "用户最近迷上了夜跑，每周三次",
            Some("夜跑"),
            1_000,
        )
        .await;

        let count = check_and_extract(
            engine.storage_ref().as_ref(),
            engine.llm_ref().as_ref(),
            &engine.config(),
            "char-0001",
        )
        .await
        .expect("达阈值触发应成功返回（失败由调用方降级，不在本用例断言错误路径）");
        assert!(count <= 1, "单条 L1 不应产出多条事件，实际 {count}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L2 事件提取的规范回复（工作 / 加班主题；供主动消息口径用例使用）。
    const L2_EVENT_JSON: &str = r#"{"events": [{"title": "频繁加班", "summary": "用户最近工作压力很大，经常加班到深夜", "keywords": "工作,加班", "confidence": 0.9}]}"#;

    /// 主动消息口径（不隔离）锁定：助手侧全部为主动消息的会话照常封存——
    /// L1 摘要计入、风格统计计入、未吸收 L1 进入 L2 提取并产出事件（L2/L3 间接打通）。
    #[tokio::test]
    async fn proactive_messages_flow_through_seal_l1_style_and_l2() {
        use crate::hooks::full_seal_hooks;
        use crate::test_support::{L1_JSON_REPLY, ScriptedLlm, engine_with_shared_scripted_llm};
        use ramaria_core::traits::StoreCrud;
        use ramaria_core::types::{Message, MessageRole, MessageSource};
        use std::sync::Arc;

        let mut config = RamariaConfig::default();
        config.thresholds.l2_trigger_count = 1;
        // 测试不等待簇间节流（生产默认 800ms）
        config.thresholds.cluster_delay_ms = 0;
        // 关闭 L3 时间线触发（0 = 不按时间触发）：本用例只锁定 L1 → L2 段
        config.thresholds.l3_trigger_days = 0;
        let llm = Arc::new(ScriptedLlm::replies(&[L1_JSON_REPLY, L2_EVENT_JSON]));
        let (engine, storage, dir) =
            engine_with_shared_scripted_llm("l2-proactive-caliber", Arc::clone(&llm), config, None)
                .await;
        seed_persona(&storage, "char-0001").await;

        // 种两条未吸收 L1（与封存摘要关键词连通，保证单簇单次提取调用）
        seed_l1(
            &storage,
            "char-0001",
            "用户最近工作压力很大，常常加班到深夜",
            Some("工作压力,加班"),
            1_000,
        )
        .await;
        seed_l1(
            &storage,
            "char-0001",
            "用户提到项目上线前每天都在加班",
            Some("工作压力,加班"),
            2_000,
        )
        .await;

        // 会话：用户消息 + 全部由主动路径生成的助手消息（is_proactive=true）
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        let mut user_msg = Message::new(
            session.id,
            MessageRole::User,
            "最近工作压力好大".to_string(),
            MessageSource::Local,
        );
        user_msg.created_at = 1_000;
        storage
            .save_message(&user_msg)
            .await
            .expect("写入用户消息应成功");
        for (i, content) in ["我在呢，愿意说说吗", "加班到这么晚，辛苦了"]
            .iter()
            .enumerate()
        {
            let mut proactive = Message::new(
                session.id,
                MessageRole::Assistant,
                content.to_string(),
                MessageSource::Online,
            )
            .with_persona_uid(Some("char-0001".to_string()))
            .with_proactive(true);
            proactive.created_at = 1_001 + i as i64;
            storage
                .save_message(&proactive)
                .await
                .expect("写入主动消息应成功");
        }

        engine.set_seal_hooks(full_seal_hooks(&engine));
        let outcome = engine.seal(session.id).await.expect("封存应成功");
        assert!(outcome.sealed, "本次调用应抢到封存权");
        assert_eq!(outcome.l1_count, 1, "含主动消息的会话应照常生成 L1");

        // ① 风格统计计入：两条主动消息进入样本（用户消息无 persona 归属，不计入）
        let stats = storage
            .get_style_stats("char-0001")
            .await
            .expect("读取风格统计应成功")
            .expect("风格统计步骤应已执行");
        assert_eq!(
            stats.sample_count, 2,
            "主动消息应计入风格统计样本（零来源过滤）"
        );

        // ② L2 间接路径打通：事件已由含主动消息的 L1 提取落库
        let events = storage
            .list_events_by_persona("char-0001", 0, 100)
            .await
            .expect("查询事件应成功");
        assert!(!events.is_empty(), "主动消息经 L1 应进入 L2 提取并产出事件");

        // ③ 调用序锁定：L1 摘要 1 次 + L2 提取 1 次（无 L3 级联）
        assert_eq!(llm.call_count(), 2, "应恰好为 L1 摘要与 L2 提取各一次调用");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
