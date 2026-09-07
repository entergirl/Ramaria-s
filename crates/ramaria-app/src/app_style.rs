//! crates/ramaria-app/src/app_style.rs - 表达层风格统计编排用例（A3）
//!
//! 设计特点:
//! - 封存钩子实现：读 persona 全部消息 → 五维统计 → 基线池更新 → 显著性检验
//!   → 规则生成（模板/LLM）→ persona_style_stats 落库 + SpeakingStyle 事实落库
//! - 注入读取：`load_style_rule` 从 persona_style_stats 读取规则文本（仅 Ready 状态）
//! - 关键词衔接：从 keyword_pool 读 canonical 词表快照注入风格统计（词典增强），
//!   读取失败/空词表静默回退纯 bigram（无词表即等价于纯 bigram，不依赖 KeywordService 服务态）
//! - 小样本原文样例兜底：样本不足不生成自动规则时，
//!   由既有消息选短样例写入 SpeakingStyle 样例事实（画像展示用），
//!   注入仍走 persona_style_stats（Insufficient 不注入，prompt 等价）
//! - 全局基线池持久化于 settings 表（键 `style_baseline_pool_v1`，JSON，不含原文）
//! - 静默降级：任一环节失败记 warn 不阻塞封存；数据不足/无显著项不生成规则
//! - 隐私红线：stats_json 与基线池只含统计参数，不含原文消息文本；样例事实仅在
//!   persona_uid 隔离的画像库内保存（不含 QQ 号，日志不落样例原文）
//! - 回归红线：`[style].enabled=false` 时本模块不执行（由调用方判断）

use ramaria_core::config::StyleConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::{
    FactSource, PersonaFact, PersonaStyleStats, ProfileField, StyleRuleSource, StyleStatsStatus,
};
use ramaria_memory::style::{BaselinePool, StyleStats, analyze_significance, generate_style_rule};

/// 全局基线池在 settings 表的存储键。
const BASELINE_POOL_KEY: &str = "style_baseline_pool_v1";

/// 风格规则 LLM 翻译增强温度（评估约定 0.3）。
const STYLE_RULE_TEMPERATURE: f64 = 0.3;

/// 小样本样例兜底的最大样例条数。
const STYLE_SAMPLE_MAX_ITEMS: usize = 3;

/// 小样本样例兜底的单条最大字符数（避免原文过长入库）。
const STYLE_SAMPLE_MAX_CHARS: usize = 48;

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
    // 1. 读取 persona 全部消息
    // 风格统计需对该 persona 全部消息一次性计算五维分布，属离线分析路径，
    // 故此处有意全量加载（浏览/展示请走 list_messages_by_persona_paginated）。
    let messages = storage.list_messages_by_persona(persona_uid).await?;

    // 2. canonical 词表（关键词衔接：风格候选与关键词体系对齐；
    //    `[style].keyword_dict=false` 或读取失败/无词表 → 回退纯 bigram，不阻塞统计）
    let canonical_words = if config.keyword_dict {
        load_canonical_keywords(storage).await
    } else {
        Vec::new()
    };

    // 3. 计算五维统计（canonical 词典增强）
    let stats = StyleStats::compute_with_keywords(&messages, config, &canonical_words);

    // 4. 加载基线池并按 persona 更新
    let mut pool = load_baseline_pool(storage).await?;
    pool.update_persona(persona_uid, &stats);

    // 5. 显著性分析 + 规则生成
    let (rule_text, rule_source, status) = match analyze_significance(&stats, &pool, config) {
        None => {
            // 数据不足：不生成规则；样例兜底写入画像事实（不注入，prompt 保持既有语义）
            if config.sample_fallback && !messages.is_empty() {
                let samples = stats.pick_style_samples(
                    &messages,
                    STYLE_SAMPLE_MAX_ITEMS,
                    STYLE_SAMPLE_MAX_CHARS,
                );
                if !samples.is_empty() {
                    let content = render_style_sample_content(&samples);
                    upsert_speaking_style_fact_if_changed(storage, persona_uid, &content).await?;
                }
            }
            (None, StyleRuleSource::None, StyleStatsStatus::Insufficient)
        }
        Some(sig) => {
            let rule = generate_style_rule(
                &stats,
                &sig,
                llm,
                config.auto_translate,
                STYLE_RULE_TEMPERATURE,
            )
            .await?;
            if rule.trim().is_empty() {
                (None, StyleRuleSource::None, StyleStatsStatus::NoSignificant)
            } else {
                // 5a. SpeakingStyle 事实落库（版本链：旧 superseded + 新 active）
                let source = if config.auto_translate && llm.is_some() {
                    StyleRuleSource::Llm
                } else {
                    StyleRuleSource::Template
                };
                upsert_speaking_style_fact(storage, persona_uid, &rule).await?;
                (Some(rule), source, StyleStatsStatus::Ready)
            }
        }
    };

    // 6a. persona_style_stats 落库（单行 upsert）
    let baseline_version = pool.n_personas() as u32;
    let stats_json = serde_json::to_string(&stats).map_err(|e| {
        tracing::warn!(error = %e, "序列化风格统计失败");
        ramaria_core::error::RamariaError::serialization("序列化风格统计失败")
    })?;
    let record = PersonaStyleStats::new(
        persona_uid.to_string(),
        stats.sample_count,
        stats_json,
        baseline_version,
        rule_text,
        rule_source,
        status,
    );
    storage.upsert_style_stats(&record).await?;

    // 6b. 持久化基线池（含原文-free 的频率摘要）
    save_baseline_pool(storage, &pool).await?;

    tracing::info!(
        persona_uid,
        sample_count = stats.sample_count,
        status = %status,
        "风格统计增量更新完成"
    );
    Ok(())
}

/// 从 persona_style_stats 读取自动风格规则文本（注入侧）。
///
/// 返回:
/// - `Ok(Some(rule))`: 状态为 Ready 且有规则文本（可注入）。
/// - `Ok(None)`: 数据不足 / 无显著项 / 风格未统计（静默跳过，prompt 与 v1.6 等价）。
/// - `Err`: 读取失败（调用方降级为 None，不阻塞对话）。
pub async fn load_style_rule(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<Option<String>> {
    match storage.get_style_stats(persona_uid).await? {
        Some(stats) if stats.status == StyleStatsStatus::Ready => {
            Ok(stats.rule_text.filter(|t| !t.trim().is_empty()))
        }
        _ => Ok(None),
    }
}

/// 读取 keyword_pool canonical 词表（关键词体系衔接）。
///
/// 说明:
/// - canonical 语义 = keyword_pool 中 `alias_status` 为 NULL/"canonical" 的词条
///   （alias/pending 归一后的规范词，与 keyword/service.rs 三态一致）。
/// - 读取失败 / 无词条 → warn 并以空表返回（调用方回退纯 bigram，
///   不阻塞封存）。词表仅作风格候选的词典增强输入，不建立 keyword→style 反向依赖。
async fn load_canonical_keywords(storage: &dyn StorageBackend) -> Vec<String> {
    let rows = match storage.list_keyword_pool_entries().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "读取 canonical 词表失败，风格统计回退纯 bigram");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter(|row| matches!(row.alias_status.as_deref(), None | Some("canonical")))
        .map(|row| row.keyword)
        .collect()
}

/// 落库 SpeakingStyle 事实（版本链：旧 active → superseded，新事实 → active）。
///
/// 说明:
/// - 与知识层事实同一版本链机制（`save_fact_with_version`），
///   知识层只读引用（检索注入已排除 SpeakingStyle，见 fact/retriever.rs）。
/// - 无旧事实时直接新增。
async fn upsert_speaking_style_fact(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    content: &str,
) -> RamariaResult<()> {
    let old = storage
        .list_active_facts_by_field(persona_uid, ProfileField::SpeakingStyle)
        .await?
        .into_iter()
        .next();
    let new_fact = PersonaFact::new(
        persona_uid.to_string(),
        ProfileField::SpeakingStyle,
        content.to_string(),
        FactSource::Event,
    );
    match old {
        Some(old_fact) => {
            storage.save_fact_with_version(&old_fact, &new_fact).await?;
        }
        None => {
            storage.save_fact(&new_fact).await?;
        }
    }
    Ok(())
}

/// 幂等落库 SpeakingStyle 事实：active 事实内容相同则不写（避免重复版本链）。
///
/// 用法:
/// - 小样本样例兜底每次封存都生成同一批样例，内容不变时跳过版本化写入。
async fn upsert_speaking_style_fact_if_changed(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    content: &str,
) -> RamariaResult<()> {
    let current = storage
        .list_active_facts_by_field(persona_uid, ProfileField::SpeakingStyle)
        .await?
        .into_iter()
        .next();
    if let Some(fact) = current
        && fact.content == content
    {
        return Ok(());
    }
    upsert_speaking_style_fact(storage, persona_uid, content).await
}

/// 渲染样例兜底文本（画像数据，含标注；非自动规则）。
///
/// 说明:
/// - 供小样本阶段 SpeakingStyle 画像保存可读的风格参考；自动规则生成后会被版本链覆盖。
/// - 标注保持中性（"历史发言为例"），避免达阈值但无显著项时"样本不足"字样过时。
fn render_style_sample_content(samples: &[String]) -> String {
    let quoted: Vec<String> = samples.iter().map(|s| format!("「{s}」")).collect();
    format!(
        "（以历史发言为例，自动统计规则暂未生成）{}",
        quoted.join("")
    )
}

/// 从 settings 表加载全局基线池（不存在 → 空池，冷启动）。
async fn load_baseline_pool(storage: &dyn StorageBackend) -> RamariaResult<BaselinePool> {
    match storage.get_setting(BASELINE_POOL_KEY).await? {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            tracing::warn!(error = %e, "反序列化风格基线池失败，使用空池重建");
            ramaria_core::error::RamariaError::serialization("反序列化风格基线池失败")
        }),
        None => Ok(BaselinePool::new()),
    }
}

/// 保存全局基线池到 settings 表。
async fn save_baseline_pool(
    storage: &dyn StorageBackend,
    pool: &BaselinePool,
) -> RamariaResult<()> {
    let json = serde_json::to_string(pool).map_err(|e| {
        tracing::warn!(error = %e, "序列化风格基线池失败");
        ramaria_core::error::RamariaError::serialization("序列化风格基线池失败")
    })?;
    storage.set_setting(BASELINE_POOL_KEY, &json).await
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
        let mut cfg = StyleConfig::default();
        cfg.sample_fallback = sample_fallback;

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
        let mut cfg = StyleConfig::default();
        cfg.keyword_dict = false; // 关闭词表衔接 → 回退纯 bigram
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
