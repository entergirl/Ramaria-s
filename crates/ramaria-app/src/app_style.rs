//! crates/ramaria-app/src/app_style.rs - 表达层风格统计编排用例（A3）
//!
//! 设计特点:
//! - 封存钩子编排：`style_incremental_update_core` 薄委托到 `ramaria_memory::style::orchestrate`
//!   （读消息 → 统计 → 基线池 → 规则生成的实现由 memory 层提供）
//! - 注入读取：`load_style_rule` 薄委托到 memory（从 persona_style_stats 读取规则文本，仅 Ready 状态）
//! - 回归红线：`[style].enabled=false` 时本模块不执行（由调用方判断）
//! - 隐私红线：注入只读取规则文本；统计产物（stats_json / 基线池）不含原文文本

use ramaria_core::config::StyleConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{LlmProvider, StorageBackend};
#[cfg(test)]
use ramaria_core::types::{FactSource, PersonaFact, ProfileField, StyleStatsStatus};
#[cfg(test)]
use ramaria_memory::style::{BaselinePool, StyleStats};

/// 执行 persona 风格统计增量更新（封存钩子，与行为层同钩子位置）。
///
/// 流程:
/// 1. 读取 persona 全部消息（`list_messages_by_persona`）。
/// 2. 读取 keyword_pool canonical 词表（关键词体系衔接；失败/空 → 回退纯 bigram）。
/// 3. 计算五维统计（`StyleStats::compute_with_keywords`，词典增强）。
/// 4. 加载全局基线池 → 按 persona 更新（增量）。
/// 5. 显著性分析 → 规则文本生成（模板优先 + LLM 增强）。
/// 6. 落库 `persona_style_stats`（单行 upsert）+ SpeakingStyle 事实（版本链）。
/// 7. 持久化基线池。
///
/// 降级（不阻塞封存）:
/// - 消息读取失败 → 错误上抛（由钩子调用方记 warn）。
/// - 基线池加载/保存失败 → 错误上抛（由钩子调用方记 warn）。
/// - canonical 词表读取失败 → warn 并以空词表继续（等价纯 bigram）。
/// - 数据不足（n_p < 阈值）→ status=Insufficient，不生成规则文本（静默跳过）；
///   若 `[style].sample_fallback` 开启且有代表性样例 → 写 SpeakingStyle 样例事实
///   （画像数据；注入仍走 persona_style_stats，Insufficient 不注入）。
/// - 无显著项 → status=NoSignificant，不生成规则文本、不写样例事实。
/// - LLM 不可用/失败 → 仅模板（静默降级链）。
///
/// 安全约束:
/// - stats_json 与基线池 JSON 均不含原文消息文本（隐私红线）。
/// - 规则文本为自动生成的风格描述（口癖词/频率），不含具体对话内容。
/// - 样例文本来自 persona 自己的历史短消息，仅在 persona_uid 隔离的画像库保存。
pub async fn style_incremental_update_core(
    storage: &dyn StorageBackend,
    llm: Option<&dyn LlmProvider>,
    config: &StyleConfig,
    persona_uid: &str,
) -> RamariaResult<()> {
    ramaria_memory::style::orchestrate::incremental_update(storage, llm, config, persona_uid).await
}

/// 从 persona_style_stats 读取自动风格规则文本（注入侧，薄委托）。
///
/// 返回:
/// - `Ok(Some(rule))`: 状态为 Ready 且有规则文本（可注入）。
/// - `Ok(None)`: 数据不足 / 无显著项 / 风格未统计（静默跳过，prompt 不含自动风格规则）。
/// - `Err`: 读取失败（调用方降级为 None，不阻塞对话）。
///
/// 说明:
/// - 实现见 `ramaria_memory::style::orchestrate::load_style_rule`（与 service / MCP 入口同源）。
pub async fn load_style_rule(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<Option<String>> {
    ramaria_memory::style::orchestrate::load_style_rule(storage, persona_uid).await
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::MockStorage;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{FactStatus, Message, MessageRole, MessageSource};
    use uuid::Uuid;

    fn msg(content: &str) -> Message {
        Message::new(
            Uuid::new_v4(),
            MessageRole::Assistant,
            content.to_string(),
            MessageSource::Local,
        )
    }

    /// 构造一批足够样本量的消息（n_p ≥ 200）供统计使用。
    fn enough_messages() -> Vec<Message> {
        let mut out = Vec::new();
        for i in 0..200 {
            let tail = if i % 2 == 0 { "哇塞" } else { "嗯嗯" };
            out.push(msg(&format!("今天也好开心啊，{tail}！看书很有意思")));
        }
        out
    }

    /// 构造样本量不足的消息（n_p < 200），含"哇塞"等非停用词。
    fn insufficient_messages() -> Vec<Message> {
        (0..150)
            .map(|i| {
                let tail = if i % 2 == 0 { "哇塞" } else { "嗯嗯" };
                msg(&format!("今天也好开心啊，{tail}！看书很有意思"))
            })
            .collect()
    }

    async fn assert_insufficient_closed_loop(sample_fallback: bool) {
        let storage = MockStorage::new();
        storage.add_persona_messages("char-0001", insufficient_messages());
        let cfg = StyleConfig {
            sample_fallback,
            ..Default::default()
        };

        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("增量更新不应失败");

        // 1. persona_style_stats 标注 Insufficient 且 rule_text=None
        let record = storage
            .get_style_stats("char-0001")
            .await
            .expect("读取统计成功")
            .expect("应有统计记录");
        assert_eq!(
            record.status,
            StyleStatsStatus::Insufficient,
            "样本不足应标注 Insufficient"
        );
        assert!(record.rule_text.is_none(), "样本不足不生成自动规则");

        // 2. 注入侧不注入（load_style_rule 仅 Ready 返回）
        let loaded = load_style_rule(&storage, "char-0001")
            .await
            .expect("加载规则成功");
        assert!(loaded.is_none(), "Insufficient 不注入自动规则");

        // 3. SpeakingStyle 画像事实按 sample_fallback 开关写入样例
        let active = storage
            .list_active_facts_by_field("char-0001", ProfileField::SpeakingStyle)
            .await
            .expect("读取事实成功");
        if sample_fallback {
            assert!(!active.is_empty(), "sample_fallback=true 时应写入样例事实");
            assert!(
                active[0].content.contains("历史发言"),
                "样例事实应含样例标注: {}",
                active[0].content
            );
        } else {
            assert!(active.is_empty(), "sample_fallback=false 时不写样例事实");
        }
    }

    #[tokio::test]
    async fn small_sample_marks_insufficient_without_rule() {
        // 回归红线：样本不足不生成自动规则、不注入（prompt 保持既有语义）
        assert_insufficient_closed_loop(true).await;
    }

    #[tokio::test]
    async fn small_sample_sample_fallback_disabled_keeps_legacy_pure() {
        // sample_fallback=false → 纯标注不写样例事实（保持既有纯标注行为）
        assert_insufficient_closed_loop(false).await;
    }

    #[tokio::test]
    async fn threshold_reached_recovers_ready_and_rule() {
        let storage = MockStorage::new();
        // 先不足：Insufficient（写入样例事实）
        storage.add_persona_messages("char-0001", insufficient_messages());
        let cfg = StyleConfig::default();
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("第一次更新成功");
        assert_eq!(
            storage
                .get_style_stats("char-0001")
                .await
                .unwrap()
                .unwrap()
                .status,
            StyleStatsStatus::Insufficient
        );

        // 达到阈值（补足 50 条 → 200）：自动恢复规则生成
        let extra: Vec<Message> = (0..50)
            .map(|_| msg("今天也好开心啊，哇塞！看书很有意思"))
            .collect();
        storage.add_persona_messages("char-0001", extra);
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("第二次更新成功");

        let record = storage
            .get_style_stats("char-0001")
            .await
            .expect("读取统计成功")
            .expect("应有统计记录");
        assert_eq!(
            record.status,
            StyleStatsStatus::Ready,
            "达阈值后应恢复 Ready"
        );
        let rule = record.rule_text.expect("达阈值后应生成自动规则");
        assert!(!rule.trim().is_empty(), "规则文本非空");

        // 注入侧可注入（自动恢复）
        let loaded = load_style_rule(&storage, "char-0001")
            .await
            .expect("加载规则成功");
        assert_eq!(loaded.as_deref(), Some(rule.as_str()), "Ready 注入自动规则");

        // SpeakingStyle 事实被规则覆盖（版本链：样例 superseded → 规则 active）
        let active = storage
            .list_active_facts_by_field("char-0001", ProfileField::SpeakingStyle)
            .await
            .expect("读取事实成功");
        assert!(
            active
                .iter()
                .any(|f| f.content.contains("口癖词") || f.content.contains("常聊")),
            "active SpeakingStyle 事实应为规则文本: {:?}",
            active
        );
    }

    #[tokio::test]
    async fn sample_fact_unchanged_does_not_spam_versions() {
        let storage = MockStorage::new();
        storage.add_persona_messages("char-0001", insufficient_messages());
        let cfg = StyleConfig::default();
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("第一次更新成功");
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("第二次更新成功（幂等）");

        let all = storage
            .list_facts_by_persona("char-0001", ProfileField::SpeakingStyle)
            .await
            .expect("读取全部事实成功");
        assert_eq!(all.len(), 1, "同内容样例不重复写版本链");
    }

    /// 冷启动空数据（persona 无任何消息）→ 不 panic、status=Insufficient、
    /// 不生成规则、不注入、不写样例事实（样例兜底需有代表性消息）。
    #[tokio::test]
    async fn empty_messages_cold_start_marks_insufficient_no_rule() {
        let storage = MockStorage::new();
        // 不添加任何消息：persona 处于零数据冷启动
        let cfg = StyleConfig::default();
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("空数据增量更新不应失败");

        let record = storage
            .get_style_stats("char-0001")
            .await
            .expect("读取统计成功")
            .expect("应有统计记录");
        assert_eq!(
            record.status,
            StyleStatsStatus::Insufficient,
            "零消息冷启动应标注 Insufficient"
        );
        assert!(record.rule_text.is_none(), "空数据不生成自动规则");

        let loaded = load_style_rule(&storage, "char-0001")
            .await
            .expect("加载规则成功");
        assert!(loaded.is_none(), "Insufficient 不注入自动规则");

        let active = storage
            .list_active_facts_by_field("char-0001", ProfileField::SpeakingStyle)
            .await
            .expect("读取事实成功");
        assert!(
            active.is_empty(),
            "无消息 → 无样例可兜底，不写 SpeakingStyle 事实"
        );
    }

    #[tokio::test]
    async fn canonical_keywords_enrich_style_stats_via_hook() {
        let storage = MockStorage::new();
        storage.seed_canonical_keyword("工作压力");
        storage.add_persona_messages(
            "char-0001",
            (0..250)
                .map(|_| msg("最近工作压力好大，晚上都在想工作压力的事"))
                .collect(),
        );
        let cfg = StyleConfig::default();
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("更新成功");

        let record = storage
            .get_style_stats("char-0001")
            .await
            .unwrap()
            .expect("应有统计");
        assert!(
            record.stats_json.contains("\"工作压力\""),
            "canonical 词应进入风格候选: {}",
            record.stats_json
        );
    }

    #[tokio::test]
    async fn keyword_dict_disabled_falls_back_to_pure_bigram() {
        let storage = MockStorage::new();
        storage.seed_canonical_keyword("工作压力");
        storage.add_persona_messages(
            "char-0001",
            (0..250)
                .map(|_| msg("最近工作压力好大，晚上都在想工作压力的事"))
                .collect(),
        );
        let cfg = StyleConfig {
            keyword_dict: false, // 关闭词表衔接 → 回退纯 bigram
            ..Default::default()
        };
        style_incremental_update_core(&storage, None, &cfg, "char-0001")
            .await
            .expect("更新成功");

        let record = storage
            .get_style_stats("char-0001")
            .await
            .unwrap()
            .expect("应有统计");
        assert!(
            !record.stats_json.contains("\"工作压力\""),
            "keyword_dict=false 时 canonical 整词不应作为候选（回退纯 bigram）: {}",
            record.stats_json
        );
        assert!(
            record.stats_json.contains("\"作压\""),
            "纯 bigram 会产出跨词噪声二元组（等价行为）"
        );
    }

    #[test]
    fn speaking_style_fact_constructed_with_event_source() {
        let fact = PersonaFact::new(
            "char-0001".into(),
            ProfileField::SpeakingStyle,
            "你习惯使用口癖词「哇塞」。".into(),
            FactSource::Event,
        );
        assert_eq!(fact.field, ProfileField::SpeakingStyle);
        assert_eq!(fact.source, FactSource::Event);
        assert_eq!(fact.status, FactStatus::Active);
    }

    #[test]
    fn stats_json_serializes_without_raw_text() {
        // 统计参数 JSON 不含原文消息文本（隐私红线）
        let messages = enough_messages();
        let cfg = StyleConfig::default();
        let stats = StyleStats::compute(&messages, &cfg);
        let json = serde_json::to_string(&stats).expect("序列化成功");
        assert!(!json.contains("今天也好开心"), "stats_json 不含原文");
        assert!(json.contains("sample_count"), "含样本量");
        assert!(json.contains("哇塞"), "含口癖词统计");
    }

    #[test]
    fn baseline_pool_json_contains_no_raw_text() {
        // 基线池 JSON 不含原文消息文本（隐私红线）
        let messages = enough_messages();
        let cfg = StyleConfig::default();
        let stats = StyleStats::compute(&messages, &cfg);
        let mut pool = BaselinePool::new();
        pool.update_persona("char-0001", &stats);
        let json = serde_json::to_string(&pool).expect("序列化成功");
        assert!(!json.contains("今天也好开心"), "基线池不含原文");
    }
}
