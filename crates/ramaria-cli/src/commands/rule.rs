//! rust/crates/ramaria-cli/src/commands/rule.rs - 行为规则管理命令（D7，v1.5 M5）
//!
//! 设计特点:
//! - 子命令遵循 §2.9 动词词表：list/show/import/edit/enable/disable/delete/evidence
//!   （`get` 仅 config 专用，规则详情用 `show`）
//! - 全部支持全局 `--json` 信封（D-V15-011）；stdout 只输出数据
//! - delete 为破坏性操作：交互确认 / 非 TTY 或 `--yes` 自动通过（M1 B 项）
//! - evidence 展示规则 → 事件 → 原文溯源链（只含结构化字段，原文不落日志）
//! - edit/disable 触发 H1 S1 反馈写入（行为层内部处理）

use anyhow::Context;
use std::sync::Arc;

use crate::json;

// =========================================================
// 公共枚举与入口
// =========================================================

/// Rule 子命令。
#[derive(Debug, Clone)]
pub enum RuleCmd {
    /// 列出 persona 的全部规则（含禁用项）
    List {
        /// 按 persona_uid 筛选
        persona: Option<String>,
        /// 输出条数上限（None = 全部）
        limit: Option<usize>,
        /// 跳过前 N 条（分页）
        offset: usize,
    },
    /// 查看单条规则详情
    Show { id: i64 },
    /// 手工导入规则（JSON 文件，`-` = stdin）
    Import {
        /// 导入源文件路径（`-` = stdin）
        file: String,
        /// 规则所属 persona
        persona: Option<String>,
    },
    /// 编辑规则（reaction / avoid；编辑后转为 Manual 并写 S1 反馈）
    Edit {
        id: i64,
        /// 新的规则文本（缺省保留原值）
        reaction: Option<String>,
        /// 新的禁忌列表（逗号分隔，缺省保留原值）
        avoid: Option<String>,
    },
    /// 启用规则
    Enable { id: i64 },
    /// 禁用规则（写 S1 反馈）
    Disable { id: i64 },
    /// 删除规则（需确认）
    Delete { id: i64, force: bool },
    /// 展示规则证据链（规则 → 事件 → 原文摘要）
    Evidence { id: i64 },
}

/// 默认规则所属 persona（与全局默认一致）。
const DEFAULT_RULE_PERSONA: &str = "rama-0001";

/// 运行 rule 子命令分发。
///
/// 参数:
/// - `app`: App 实例引用。
/// - `cmd`: Rule 子命令。
/// - `json`: JSON 信封输出。
/// - `yes`: 自动确认所有确认点（delete 等）。
pub async fn run(
    app: &Arc<ramaria_app::App>,
    cmd: RuleCmd,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    match cmd {
        RuleCmd::List {
            persona,
            limit,
            offset,
        } => run_list(app, persona, limit, offset, json).await,
        RuleCmd::Show { id } => run_show(app, id, json).await,
        RuleCmd::Import { file, persona } => run_import(app, &file, persona, json).await,
        RuleCmd::Edit {
            id,
            reaction,
            avoid,
        } => run_edit(app, id, reaction, avoid, json).await,
        RuleCmd::Enable { id } => run_set_enabled(app, id, true, json).await,
        RuleCmd::Disable { id } => run_set_enabled(app, id, false, json).await,
        RuleCmd::Delete { id, force } => run_delete(app, id, force, json, yes).await,
        RuleCmd::Evidence { id } => run_evidence(app, id, json).await,
    }
}

// =========================================================
// list
// =========================================================

/// 列出 persona 的行为规则。
async fn run_list(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    limit: Option<usize>,
    offset: usize,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());
    let mut rules = ramaria_app::commands::behavior::behavior_list_rules(app, &persona_uid)
        .await
        .context("查询行为规则失败")?;

    // 分页（列表命令统一 --limit/--offset 约定，T-V15-1-006）
    let total = rules.len();
    if let Some(limit) = limit {
        rules = rules.into_iter().skip(offset).take(limit).collect();
    } else if offset > 0 {
        rules = rules.into_iter().skip(offset).collect();
    }

    if json {
        // 结构化输出：规则列表 + 分页前总数（与 L1/L2/L3 一致）
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "total": total,
            "rules": rules,
        });
        return json::emit_ok(&data);
    }

    if rules.is_empty() {
        crate::ui::info(&format!("人格 {persona_uid} 暂无行为规则"));
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled("规则数", &format!("{total}"));
    crate::ui::separator();
    for rule in &rules {
        let status = if rule.enabled { "启用" } else { "禁用" };
        let source = if rule.source == ramaria_core::behavior::RuleSource::Manual {
            "Manual"
        } else {
            "Auto"
        };
        let reaction = rule
            .reaction
            .as_deref()
            .unwrap_or("（候选规则，仅参数注入）");
        println!("#{} [{}] [{}] {reaction}", rule.id, source, status);
        println!("   情境: {}", rule.situation.keywords.join("、"));
        println!(
            "   置信度 {:.2} · 稳定性 {:.2} · 证据 {} 条",
            rule.confidence,
            rule.stability,
            rule.evidence.len()
        );
    }
    Ok(())
}

// =========================================================
// show
// =========================================================

/// 查看单条规则详情。
async fn run_show(app: &Arc<ramaria_app::App>, id: i64, json: bool) -> anyhow::Result<()> {
    let rule = ramaria_app::commands::behavior::behavior_get_rule(app, id)
        .await
        .context("查询行为规则失败")?
        .ok_or_else(|| anyhow::anyhow!("行为规则 {id} 不存在"))?;

    if json {
        return json::emit_ok(&rule);
    }

    crate::ui::separator();
    crate::ui::labeled("ID", &rule.id.to_string());
    crate::ui::labeled("Persona", &rule.persona_uid);
    crate::ui::labeled(
        "来源",
        if rule.source == ramaria_core::behavior::RuleSource::Manual {
            "Manual（人工）"
        } else {
            "Auto（自动学习）"
        },
    );
    crate::ui::labeled("状态", if rule.enabled { "启用" } else { "禁用" });
    crate::ui::labeled(
        "规则文本",
        rule.reaction
            .as_deref()
            .unwrap_or("（候选规则，仅参数注入）"),
    );
    crate::ui::labeled("情境关键词", &rule.situation.keywords.join("、"));
    crate::ui::labeled(
        "情感强度",
        &format!("{:.2}", rule.params.emotional_intensity),
    );
    crate::ui::labeled("主动程度", &format!("{:.2}", rule.params.proactiveness));
    crate::ui::labeled("详细度", &format!("{:.2}", rule.params.detail_level));
    crate::ui::labeled("正式度", &format!("{:.2}", rule.params.formality));
    let avoid_display = if rule.avoid.is_empty() {
        "（无）".to_string()
    } else {
        rule.avoid.join("、")
    };
    crate::ui::labeled("禁忌列表", &avoid_display);
    crate::ui::labeled("置信度", &format!("{:.2}", rule.confidence));
    crate::ui::labeled("稳定性", &format!("{:.2}", rule.stability));
    crate::ui::labeled("证据数", &rule.evidence.len().to_string());
    crate::ui::separator();
    Ok(())
}

// =========================================================
// import
// =========================================================

/// 手工导入规则（JSON 校验在行为层执行）。
async fn run_import(
    app: &Arc<ramaria_app::App>,
    file: &str,
    persona: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());
    // `-` = stdin；否则读文件
    let content = if file == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("读取 stdin 失败")?;
        buf
    } else {
        std::fs::read_to_string(file).with_context(|| format!("读取文件 {file} 失败"))?
    };

    let id = ramaria_app::commands::behavior::behavior_import_rule(app, &persona_uid, &content)
        .await
        .context("规则导入校验失败")?;

    if json {
        let data = serde_json::json!({
            "id": id,
            "persona_uid": persona_uid,
            "source": "manual",
        });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 导入成功（Manual，自动生效）"));
    Ok(())
}

// =========================================================
// edit / enable / disable
// =========================================================

/// 编辑规则（reaction / avoid；编辑后转为 Manual 并写 S1 反馈）。
async fn run_edit(
    app: &Arc<ramaria_app::App>,
    id: i64,
    reaction: Option<String>,
    avoid: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    if reaction.is_none() && avoid.is_none() {
        anyhow::bail!("请至少提供 --reaction 或 --avoid 之一");
    }
    let mut rule = ramaria_app::commands::behavior::behavior_get_rule(app, id)
        .await
        .context("查询行为规则失败")?
        .ok_or_else(|| anyhow::anyhow!("行为规则 {id} 不存在"))?;

    if let Some(r) = reaction {
        let trimmed = r.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("--reaction 不能为空（候选规则请用 import 导入 params）");
        }
        rule.reaction = Some(trimmed);
    }
    if let Some(a) = avoid {
        rule.avoid = a
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    ramaria_app::commands::behavior::behavior_edit_rule(app, &mut rule, None)
        .await
        .context("编辑行为规则失败")?;

    if json {
        let data = serde_json::json!({
            "id": id,
            "source": "manual",
            "reaction": rule.reaction,
            "avoid": rule.avoid,
        });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 已编辑（转为 Manual，S1 反馈已记录）"));
    Ok(())
}

/// 启用/禁用规则（disable 写 S1 反馈）。
async fn run_set_enabled(
    app: &Arc<ramaria_app::App>,
    id: i64,
    enabled: bool,
    json: bool,
) -> anyhow::Result<()> {
    ramaria_app::commands::behavior::behavior_set_rule_enabled(app, id, enabled, None)
        .await
        .context("切换规则状态失败")?;

    let action = if enabled { "启用" } else { "禁用" };
    if json {
        let data = serde_json::json!({ "id": id, "enabled": enabled });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 已{action}"));
    Ok(())
}

// =========================================================
// delete
// =========================================================

/// 删除规则（破坏性操作：确认 / --yes / --force）。
async fn run_delete(
    app: &Arc<ramaria_app::App>,
    id: i64,
    force: bool,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    // 先确认规则存在（避免删除不存在的 id 静默成功）
    ramaria_app::commands::behavior::behavior_get_rule(app, id)
        .await
        .context("查询行为规则失败")?
        .ok_or_else(|| anyhow::anyhow!("行为规则 {id} 不存在"))?;

    let confirmed = force
        || crate::ui::confirm(&format!("确定删除行为规则 #{id} 吗？"), yes)
            .map_err(|e| anyhow::anyhow!(e))?;
    if !confirmed {
        if json {
            // 用户取消：业务校验失败（exit code 4），消息走 stderr
            json::emit_err(4, &format!("删除规则 #{id} 已取消"));
            return Ok(());
        }
        crate::ui::info(&format!("删除规则 #{id} 已取消"));
        return Ok(());
    }

    ramaria_app::commands::behavior::behavior_delete_rule(app, id)
        .await
        .context("删除行为规则失败")?;

    if json {
        let data = serde_json::json!({ "id": id, "deleted": true });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 已删除"));
    Ok(())
}

// =========================================================
// evidence
// =========================================================

/// 展示规则证据链（规则 → 事件 → 原文摘要，只含结构化字段）。
async fn run_evidence(app: &Arc<ramaria_app::App>, id: i64, json: bool) -> anyhow::Result<()> {
    let items = ramaria_app::commands::behavior::behavior_rule_evidence(app, id)
        .await
        .context("查询规则证据失败")?;

    if json {
        let data = serde_json::json!({
            "rule_id": id,
            "evidence": items,
        });
        return json::emit_ok(&data);
    }

    if items.is_empty() {
        crate::ui::info(&format!("规则 #{id} 暂无证据（手工导入规则通常无证据链）"));
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("规则 ID", &id.to_string());
    crate::ui::labeled("证据条数", &items.len().to_string());
    crate::ui::separator();
    for (i, item) in items.iter().enumerate() {
        println!(
            "[{}] 事件 #{} (权重 {:.2})",
            i + 1,
            item.event_id,
            item.weight
        );
        println!("    标题: {}", item.title);
        println!("    摘要: {}", item.summary);
        if let Some(p) = &item.paraphrase {
            println!("    态度（脱敏）: {p}");
        }
        if let Some(kw) = &item.keywords {
            println!("    关键词: {kw}");
        }
    }
    crate::ui::separator();
    Ok(())
}
