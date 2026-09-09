//! crates/ramaria-cli/src/commands/style.rs - 表达层风格统计管理命令
//!
//! 设计特点:
//! - 子命令遵循动词词表：update —— 手动补跑 persona 表达层风格统计
//!   （复用 app 层 `style_incremental_update_core`，与会话封存钩子同一核心）
//! - 背景：风格统计日常由"会话封存钩子"驱动；QQ 导入等不触发封存的路径
//!   从未生成自动风格规则，需手动补跑（覆盖无封存来源的 persona）
//! - 核心函数返回 `()`，命令在调用后读回 `persona_style_stats` 展示结果
//!   （sample_count / status / rule_source / rule_text）
//! - 全部支持全局 `--json` 信封；stdout 只输出数据
//! - 不改动 app_style.rs 既有逻辑：仅复用核心函数与读回统计
//!
//! 安全约束:
//! - 输出仅含统计参数与自动规则文本（不含消息原文，与 app_style 隐私红线一致）

use anyhow::Context;
use std::sync::Arc;

use crate::json;

// =========================================================
// 公共枚举与入口
// =========================================================

/// Style 子命令。
#[derive(Debug, Clone)]
pub enum StyleCmd {
    /// 触发 persona 风格统计增量更新（基于全部消息重新计算五维并生成/替换自动风格规则）
    Update {
        /// 目标 persona_uid
        persona: Option<String>,
    },
}

/// 默认风格统计所属 persona（与全局默认一致）。
const DEFAULT_STYLE_PERSONA: &str = "rama-0001";

/// 运行 style 子命令分发。
///
/// 参数:
/// - `app`: App 实例引用。
/// - `cmd`: Style 子命令。
/// - `json`: JSON 信封输出。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: StyleCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        StyleCmd::Update { persona } => run_update(app, persona, json).await,
    }
}

// =========================================================
// update
// =========================================================

/// 触发 persona 风格统计增量更新，并读回统计结果展示。
///
/// 说明:
/// - 复用 app 层 `style_incremental_update_core`（全量消息 → 五维统计 → 基线
///   显著性 → 规则生成/替换落库），与封存钩子同一实现，幂等可重复执行。
/// - LLM 以 `llm_clone()` 锁外克隆传递（同 app 封存钩子的 `Some(llm)` 模式）；
///   LLM 不可用/失败由核心内部静默降级为模板生成，不阻塞命令。
/// - 空数据/无显著项不报错：落库 status=Insufficient / NoSignificant 并正常返回。
/// - 核心返回 `()`，此处调用后读回 `persona_style_stats` 展示更新结果。
async fn run_update(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_STYLE_PERSONA.to_string());

    let storage = Arc::clone(app.storage());
    let llm = app.llm_clone();
    let config = app.config().style.clone();
    ramaria_app::app_style::style_incremental_update_core(
        storage.as_ref(),
        Some(llm.as_ref()),
        &config,
        &persona_uid,
    )
    .await
    .context("风格统计增量更新失败")?;

    // 读回统计记录（核心已 upsert 成功，正常必有记录）
    let stats = storage
        .get_style_stats(&persona_uid)
        .await
        .context("读取风格统计结果失败")?
        .ok_or_else(|| anyhow::anyhow!("风格统计更新完成后未找到 {persona_uid} 的统计记录"))?;

    if json {
        // 结构化输出：统计状态/规则来源/规则文本（不含 stats_json 原文-free 参数明细）
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "sample_count": stats.sample_count,
            "status": stats.status.as_str(),
            "rule_source": stats.rule_source.as_str(),
            "rule_text": stats.rule_text,
            "baseline_version": stats.baseline_version,
        });
        return json::emit_ok(&data);
    }

    let status_label = match stats.status {
        ramaria_core::types::StyleStatsStatus::Insufficient => {
            "Insufficient（数据不足，未生成规则）"
        }
        ramaria_core::types::StyleStatsStatus::Ready => "Ready（规则已生成，可注入）",
        ramaria_core::types::StyleStatsStatus::NoSignificant => {
            "NoSignificant（样本足够但无显著项）"
        }
    };
    let source_label = match stats.rule_source {
        ramaria_core::types::StyleRuleSource::None => "未生成",
        ramaria_core::types::StyleRuleSource::Template => "Template（模板生成）",
        ramaria_core::types::StyleRuleSource::Llm => "LLM（翻译增强）",
    };

    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled("样本量", &stats.sample_count.to_string());
    crate::ui::labeled("状态", status_label);
    crate::ui::labeled("规则来源", source_label);
    crate::ui::labeled("基线版本", &stats.baseline_version.to_string());
    match &stats.rule_text {
        Some(rule) => {
            crate::ui::labeled("自动规则", "（已生成，供表达层注入）");
            println!("{rule}");
        }
        None => {
            crate::ui::labeled("自动规则", "（未生成：数据不足或无显著项，不注入）");
        }
    }
    crate::ui::separator();
    crate::ui::success(&format!("人格 {persona_uid} 风格统计增量更新完成"));
    Ok(())
}
