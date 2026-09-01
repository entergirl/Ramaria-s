//! crates/ramaria-app/src/app_style.rs - 表达层风格统计编排用例（A3）
//!
//! 设计特点:
//! - 封存钩子实现：读 persona 全部消息 → 五维统计 → 基线池更新 → 显著性检验
//!   → 规则生成（模板/LLM）→ persona_style_stats 落库 + SpeakingStyle 事实落库
//! - 注入读取：`load_style_rule` 从 persona_style_stats 读取规则文本（仅 Ready 状态）
//! - 全局基线池持久化于 settings 表（键 `style_baseline_pool_v1`，JSON，不含原文）
//! - 静默降级：任一环节失败记 warn 不阻塞封存；数据不足/无显著项不生成规则
//! - 隐私红线：stats_json 与基线池只含统计参数，不含原文消息文本
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

/// 执行 persona 风格统计增量更新（封存钩子，与行为层同钩子位置）。
///
/// 流程:
/// 1. 读取 persona 全部消息（`list_messages_by_persona`）。
/// 2. 计算五维统计（`StyleStats::compute`）。
/// 3. 加载全局基线池 → 按 persona 更新（增量）。
/// 4. 显著性分析 → 规则文本生成（模板优先 + LLM 增强）。
/// 5. 落库 `persona_style_stats`（单行 upsert）+ SpeakingStyle 事实（版本链）。
/// 6. 持久化基线池。
///
/// 降级（不阻塞封存）:
/// - 消息读取失败 → 错误上抛（由钩子调用方记 warn）。
/// - 基线池加载/保存失败 → 错误上抛（由钩子调用方记 warn）。
/// - 数据不足（n_p < 阈值）→ status=Insufficient，不生成规则文本（静默跳过）。
/// - 无显著项 → status=NoSignificant，不生成规则文本。
/// - LLM 不可用/失败 → 仅模板（静默降级链）。
///
/// 安全约束:
/// - stats_json 与基线池 JSON 均不含原文消息文本（隐私红线）。
/// - 规则文本为自动生成的风格描述（口癖词/频率），不含具体对话内容。
pub async fn style_incremental_update_core(
    storage: &dyn StorageBackend,
    llm: Option<&dyn LlmProvider>,
    config: &StyleConfig,
    persona_uid: &str,
) -> RamariaResult<()> {
    // 1. 读取 persona 全部消息
    let messages = storage.list_messages_by_persona(persona_uid).await?;

    // 2. 计算五维统计
    let stats = StyleStats::compute(&messages, config);

    // 3. 加载基线池并按 persona 更新
    let mut pool = load_baseline_pool(storage).await?;
    pool.update_persona(persona_uid, &stats);

    // 4. 显著性分析 + 规则生成
    let (rule_text, rule_source, status) = match analyze_significance(&stats, &pool, config) {
        None => (None, StyleRuleSource::None, StyleStatsStatus::Insufficient),
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

    // 5b. persona_style_stats 落库（单行 upsert）
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

    // 6. 持久化基线池（含原文-free 的频率摘要）
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

/// 落库 SpeakingStyle 事实（版本链：旧 active → superseded，新事实 → active）。
///
/// 说明:
/// - 与知识层事实同一版本链机制（`save_fact_with_version`），
///   知识层只读引用（检索注入已排除 SpeakingStyle，见 fact/retriever.rs）。
/// - 无旧事实时直接新增。
async fn upsert_speaking_style_fact(
    storage: &dyn StorageBackend,
    persona_uid: &str,
    rule: &str,
) -> RamariaResult<()> {
    let old = storage
        .list_active_facts_by_field(persona_uid, ProfileField::SpeakingStyle)
        .await?
        .into_iter()
        .next();
    let new_fact = PersonaFact::new(
        persona_uid.to_string(),
        ProfileField::SpeakingStyle,
        rule.to_string(),
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
