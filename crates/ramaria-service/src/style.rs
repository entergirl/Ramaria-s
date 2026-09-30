//! crates/ramaria-service/src/style.rs - 表达风格统计用例（更新 / 读取）
//!
//! 设计特点:
//! - 封存钩子编排：`incremental_update_core` 薄委托到 `ramaria_memory::style::orchestrate`
//!   （读消息 → 统计 → 基线池 → 规则生成的实现由 memory 层提供）
//! - 注入读取：`load_style_rule` 薄委托 memory（从 persona_style_stats 读取规则文本，仅 Ready 状态）
//! - 统计视图：桌面展示字段（样本量 / 状态与标签 / 规则来源与标签 / 统计 JSON / 更新时间）
//!   逐字映射，空 persona 显式拒绝
//! - 开关由调用方判断：`[style].enabled=false` 时封存钩子不调用本模块
//! - 隐私红线：注入只读取规则文本；统计产物（stats_json / 基线池）不含原文文本
//!
//! 安全约束:
//! - 视图只含统计参数与自动规则文本，不含原文消息
//! - 规则文本为自动生成的风格描述（口癖词 / 频率），不含具体对话内容

use ramaria_core::config::StyleConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{LlmProvider, StorageBackend};
use serde::Serialize;

use crate::engine::Engine;

// =========================================================
// 增量更新（封存钩子，与行为层同钩子位置）
// =========================================================

/// 执行 persona 风格统计增量更新（封存钩子核心，供宿主手动补跑）。
///
/// 流程:
/// 1. 读取 persona 全部消息（`list_messages_by_persona`）。
/// 2. 读取 keyword_pool canonical 词表（关键词体系衔接；失败 / 空 → 回退纯 bigram）。
/// 3. 计算五维统计（`StyleStats::compute_with_keywords`，词典增强）。
/// 4. 加载全局基线池 → 按 persona 更新（增量）。
/// 5. 显著性分析 → 规则文本生成（模板优先 + LLM 增强）。
/// 6. 落库 `persona_style_stats`（单行 upsert）+ SpeakingStyle 事实（版本链）。
/// 7. 持久化基线池。
///
/// 降级（不阻塞封存）:
/// - 消息读取失败 → 错误上抛（由钩子调用方记 warn）。
/// - 基线池加载 / 保存失败 → 错误上抛（由钩子调用方记 warn）。
/// - canonical 词表读取失败 → warn 并以空词表继续（等价纯 bigram）。
/// - 数据不足（n_p < 阈值）→ status=Insufficient，不生成规则文本（静默跳过）；
///   若 `[style].sample_fallback` 开启且有代表性样例 → 写 SpeakingStyle 样例事实
///   （画像数据；注入仍走 persona_style_stats，Insufficient 不注入）。
/// - 无显著项 → status=NoSignificant，不生成规则文本、不写样例事实。
/// - LLM 不可用 / 失败 → 仅模板（静默降级链）。
///
/// 安全约束:
/// - stats_json 与基线池 JSON 均不含原文消息文本（隐私红线）。
/// - 规则文本为自动生成的风格描述（口癖词 / 频率），不含具体对话内容。
/// - 样例文本来自 persona 自己的历史短消息，仅在 persona_uid 隔离的画像库保存。
pub(crate) async fn incremental_update_core(
    storage: &dyn StorageBackend,
    llm: Option<&dyn LlmProvider>,
    config: &StyleConfig,
    persona_uid: &str,
) -> RamariaResult<()> {
    ramaria_memory::style::orchestrate::incremental_update(storage, llm, config, persona_uid).await
}

/// 执行一次 persona 风格统计增量更新（取引擎依赖后委托核心）。
///
/// 返回:
/// - 更新成功返回 `Ok(())`；数据不足 / 无显著项不报错（落库对应状态）。
pub(crate) async fn incremental_update(engine: &Engine, persona_uid: &str) -> RamariaResult<()> {
    let llm = engine.llm_ref();
    let config = engine.config();
    incremental_update_core(
        engine.storage_ref().as_ref(),
        Some(llm.as_ref()),
        &config.style,
        persona_uid,
    )
    .await
}

// =========================================================
// 注入侧读取与统计视图
// =========================================================

/// 从 persona_style_stats 读取自动风格规则文本（注入侧，薄委托）。
///
/// 返回:
/// - `Ok(Some(rule))`: 状态为 Ready 且有规则文本（可注入）。
/// - `Ok(None)`: 数据不足 / 无显著项 / 风格未统计（静默跳过，prompt 不含自动风格规则）。
/// - `Err`: 读取失败（调用方降级为 None，不阻塞对话）。
pub(crate) async fn load_style_rule(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<Option<String>> {
    ramaria_memory::style::orchestrate::load_style_rule(storage, persona_uid).await
}

/// 风格统计状态中文标签映射。
fn status_label(status: &str) -> &'static str {
    match status {
        "ready" => "就绪（可注入）",
        "no_significant" => "样本充足但无显著项",
        _ => "数据不足",
    }
}

/// 规则来源中文标签映射。
fn source_label(source: &str) -> &'static str {
    match source {
        "template" => "模板生成",
        "llm" => "LLM 增强",
        _ => "未生成",
    }
}

/// 说话风格统计只读视图（字段与桌面展示口径逐字一致）。
#[derive(Debug, Clone, Serialize)]
pub struct StyleStatsView {
    /// 人格标识
    pub persona_uid: String,
    /// 统计样本量 n_p（消息条数）
    pub sample_count: u32,
    /// 全局基线池合并版本
    pub baseline_version: u32,
    /// 状态标识: insufficient / ready / no_significant
    pub status: String,
    /// 状态中文标签（供前端直接展示）
    pub status_label: String,
    /// 规则来源: none / template / llm
    pub rule_source: String,
    /// 规则来源中文标签
    pub rule_source_label: String,
    /// 自动风格规则文本（null = 未生成）
    pub rule_text: Option<String>,
    /// 五维统计 JSON 原始文本（前端解析展示）
    pub stats_json: String,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

/// 查询指定人格的说话风格统计（只读）。
///
/// 参数:
/// - `persona_uid`: 目标人格 UID。
///
/// 返回:
/// - 无记录时返回 `None`（空态，非错误）；记录存在返回完整统计视图。
/// - 空 UID 返回业务校验错误（调用方错传参数）。
pub(crate) async fn stats(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<Option<StyleStatsView>> {
    if persona_uid.trim().is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }

    let stats = engine.storage_ref().get_style_stats(persona_uid).await?;
    let view = stats.map(|s| {
        let status = s.status.as_str();
        let source = s.rule_source.as_str();
        StyleStatsView {
            persona_uid: s.persona_uid,
            sample_count: s.sample_count,
            baseline_version: s.baseline_version,
            status: status.to_string(),
            status_label: status_label(status).to_string(),
            rule_source: source.to_string(),
            rule_source_label: source_label(source).to_string(),
            rule_text: s.rule_text,
            stats_json: s.stats_json,
            updated_at: s.updated_at,
        }
    });

    tracing::debug!(%persona_uid, present = view.is_some(), "风格统计读取完成");
    Ok(view)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use crate::test_support::{engine_with_db, seed_persona, seed_session_with_messages};

    /// 样本不足（n_p < 阈值）→ Insufficient、不生成规则文本、不注入；视图标签口径锁定。
    #[tokio::test]
    async fn insufficient_sample_marks_no_rule_and_view_labels() {
        let (engine, storage, dir) = engine_with_db("style-insufficient").await;
        seed_persona(&storage, "char-0001").await;
        // 150 条消息 < 默认阈值 200 → 数据不足
        seed_session_with_messages(&storage, "char-0001", 150, 1_000).await;

        engine
            .style_incremental_update("char-0001")
            .await
            .expect("风格增量更新应成功");

        let view = engine
            .style_stats("char-0001")
            .await
            .expect("统计读取应成功")
            .expect("更新后应有统计记录");
        assert_eq!(view.persona_uid, "char-0001");
        assert_eq!(view.sample_count, 150);
        assert_eq!(view.status, "insufficient");
        assert_eq!(view.status_label, "数据不足");
        assert_eq!(view.rule_source, "none");
        assert_eq!(view.rule_source_label, "未生成");
        assert!(view.rule_text.is_none(), "样本不足不生成规则文本");
        assert!(view.updated_at > 0, "应有更新时间");

        let loaded = engine
            .style_load_rule("char-0001")
            .await
            .expect("规则读取应成功");
        assert!(loaded.is_none(), "Insufficient 不注入自动规则");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 统计读取：空 persona 拒绝（与入口校验口径一致）。
    #[tokio::test]
    async fn stats_rejects_blank_persona() {
        let (engine, _storage, dir) = engine_with_db("style-blank").await;

        let err = engine
            .style_stats("   ")
            .await
            .expect_err("空 persona 应拒绝");
        assert!(
            err.to_string().contains("人格 UID 不能为空"),
            "错误信息应保留入口口径: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未统计过：返回 None（空态，非错误）。
    #[tokio::test]
    async fn stats_absent_returns_none() {
        let (engine, _storage, dir) = engine_with_db("style-absent").await;

        let view = engine
            .style_stats("char-9999")
            .await
            .expect("统计读取应成功");
        assert!(view.is_none(), "无记录应返回空态");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
