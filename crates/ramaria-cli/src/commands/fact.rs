//! crates/ramaria-cli/src/commands/fact.rs - 知识层事实查询命令
//!
//! 设计特点:
//! - 子命令词表：list / show（**无 delete**，双端均不做事实删除）
//! - list: 按 `--persona` 与可选 `--field` 过滤，输出 active 事实卡片
//! - show: 单条事实详情 + 完整版本链（历史版本折叠展示）
//! - 全部支持全局 `--json` 信封（统一信封 schema，stdout 只输出数据）
//! - 遵循隐私约定：输出用事实陈述（非原文），日志不含事实全文

use anyhow::Context;
use std::sync::Arc;

use crate::json;
use ramaria_core::types::{FactSource, FactStatus, ProfileField};

// =========================================================
// 公共枚举与入口
// =========================================================

/// Fact 子命令（**无 delete**，双端不做事实删除）。
#[derive(Debug, Clone)]
pub enum FactCmd {
    /// 列出 persona 的 active 事实（按 field 分组）
    List {
        /// 按 persona_uid 过滤（默认 rama-0001）
        persona: Option<String>,
        /// 按 field 过滤（basic_info/personal_status/interests/social/history/recent_context/speaking_style）
        field: Option<String>,
        /// 输出条数上限
        limit: Option<usize>,
        /// 跳过前 N 条
        offset: usize,
    },
    /// 查看单条事实详情（含版本链）
    Show { id: i64 },
}

/// 默认事实所属 persona（与全局默认一致）。
const DEFAULT_FACT_PERSONA: &str = "rama-0001";

/// 运行 fact 子命令分发。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: FactCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        FactCmd::List {
            persona,
            field,
            limit,
            offset,
        } => run_list(app, persona, field, limit, offset, json).await,
        FactCmd::Show { id } => run_show(app, id, json).await,
    }
}

// =========================================================
// field 解析
// =========================================================

/// 将 CLI `--field` 字符串解析为 ProfileField（返回错误信息由调用方拼接）。
fn parse_field(s: &str) -> anyhow::Result<ProfileField> {
    match s.trim() {
        "basic_info" => Ok(ProfileField::BasicInfo),
        "personal_status" => Ok(ProfileField::PersonalStatus),
        "interests" => Ok(ProfileField::Interests),
        "social" => Ok(ProfileField::Social),
        "history" => Ok(ProfileField::History),
        "recent_context" => Ok(ProfileField::RecentContext),
        "speaking_style" => Ok(ProfileField::SpeakingStyle),
        other => anyhow::bail!(
            "未知 field: {other}（可选 basic_info/personal_status/interests/social/history/recent_context/speaking_style）"
        ),
    }
}

// =========================================================
// list
// =========================================================

/// 列出 persona 的 active 事实。
async fn run_list(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    field: Option<String>,
    limit: Option<usize>,
    offset: usize,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_FACT_PERSONA.to_string());
    // 解析 field（可选）
    let field_parsed = match field.as_deref() {
        Some(f) => Some(parse_field(f).with_context(|| "解析 --field 失败")?),
        None => None,
    };

    let mut facts = ramaria_app::commands::fact::fact_list(app, &persona_uid, field_parsed)
        .await
        .context("查询知识事实失败")?;

    let total = facts.len();
    if let Some(limit) = limit {
        facts = facts.into_iter().skip(offset).take(limit).collect();
    } else if offset > 0 {
        facts = facts.into_iter().skip(offset).collect();
    }

    if json {
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "total": total,
            "facts": facts,
        });
        return json::emit_ok(&data);
    }

    // 按 field 分组打印（知识卡片：field 徽标 + content + confidence/source）
    if facts.is_empty() {
        crate::ui::info(&format!("人格 {persona_uid} 暂无 active 知识事实"));
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled("事实数", &total.to_string());
    crate::ui::separator();
    let mut current_field: Option<ProfileField> = None;
    for f in &facts {
        if current_field != Some(f.field) {
            current_field = Some(f.field);
            println!("[{}]", f.field.label());
        }
        println!(
            "  #{} [{} · {:.0}%] {}",
            f.id,
            source_label(f.source),
            f.confidence * 100.0,
            f.content
        );
    }
    crate::ui::separator();
    Ok(())
}

// =========================================================
// show
// =========================================================

/// 查看单条事实详情（含版本链）。
async fn run_show(app: &Arc<ramaria_app::App>, id: i64, json: bool) -> anyhow::Result<()> {
    let fact = ramaria_app::commands::fact::fact_get(app, id)
        .await
        .context("查询知识事实失败")?
        .ok_or_else(|| {
            // 业务校验失败：事实不存在（exit code 4，见 main.rs::exit_code_for_error）
            ramaria_core::error::RamariaError::validation(format!("知识事实 {id} 不存在"))
        })?;

    let versions = ramaria_app::commands::fact::fact_versions(app, id)
        .await
        .context("查询事实版本链失败")?;

    if json {
        let data = serde_json::json!({
            "fact": fact,
            "versions": versions,
        });
        return json::emit_ok(&data);
    }

    crate::ui::separator();
    crate::ui::labeled("ID", &fact.id.to_string());
    crate::ui::labeled("Persona", &fact.persona_uid);
    crate::ui::labeled("字段", fact.field.label());
    crate::ui::labeled("状态", status_label(fact.status));
    crate::ui::labeled("来源", source_label(fact.source));
    crate::ui::labeled("置信度", &format!("{:.2}", fact.confidence));
    crate::ui::labeled("内容", &fact.content);
    if let Some(kw) = &fact.keyword_hint {
        crate::ui::labeled("关键词", kw);
    }
    if let Some(vo) = fact.version_of {
        crate::ui::labeled("被覆盖版本", &vo.to_string());
    }
    if let Some(re) = fact.ref_event_id {
        crate::ui::labeled("来源事件", &re.to_string());
    }
    crate::ui::separator();
    // 版本链展示（历史 → 当前）
    if versions.len() > 1 {
        crate::ui::labeled("版本链", &format!("共 {} 版", versions.len()));
        crate::ui::separator();
        for (i, v) in versions.iter().enumerate() {
            let marker = if v.id == id { "← 当前" } else { "" };
            println!("[{}] #{} {} {marker}", i + 1, v.id, status_label(v.status));
            println!("    内容: {}", v.content);
        }
        crate::ui::separator();
    }
    Ok(())
}

// =========================================================
// 辅助
// =========================================================

fn source_label(s: FactSource) -> &'static str {
    match s {
        FactSource::Manual => "manual",
        FactSource::Event => "event",
        FactSource::L1 | _ => "l1",
    }
}

fn status_label(s: FactStatus) -> &'static str {
    match s {
        FactStatus::Active => "active",
        FactStatus::Superseded => "superseded",
        FactStatus::Candidate | _ => "candidate",
    }
}
