//! crates/ramaria-cli/src/commands/memory.rs - 记忆查看命令
//!
//! 设计特点:
//! - 支持 L1（摘要）/ L2（事件）/ L3（性格）三层记忆查看
//! - 层级别名双支持: l1↔summary / l2↔events / l3↔profile，纠错提示同时列出
//! - 默认显示 L1，默认 persona 为 rama-0001
//! - --persona 筛选特定 persona 的记忆
//! - --limit/--offset 控制分页
//! - --json 输出信封（时间戳 ISO-8601 UTC），文本模式表格化展示

use anyhow::Context;
use ramaria_core::error::RamariaError;
use std::sync::Arc;

/// memory 命令参数。
pub struct MemoryArgs {
    /// 记忆层级: l1|summary / l2|events / l3|profile
    pub layer: String,
    /// 按 persona_uid 筛选
    pub persona: Option<String>,
    /// 输出条数上限
    pub limit: usize,
    /// 跳过前 N 条（分页）
    pub offset: usize,
    /// JSON 信封输出
    pub json: bool,
}

/// 解析层级别名（l1↔summary / l2↔events / l3↔profile）。
///
/// 返回:
/// - `Some(canonical)`: 合法的层级（l1/l2/l3）。
/// - `None`: 未知层级。
fn resolve_layer(layer: &str) -> Option<&'static str> {
    match layer {
        "l1" | "summary" => Some("l1"),
        "l2" | "events" => Some("l2"),
        "l3" | "profile" => Some("l3"),
        _ => None,
    }
}

/// 执行 memory 命令。
pub async fn run(app: &Arc<ramaria_app::App>, args: MemoryArgs) -> anyhow::Result<()> {
    let canonical = match resolve_layer(&args.layer) {
        Some(l) => l,
        None => {
            // 业务校验失败
            return Err(anyhow::anyhow!(RamariaError::validation(format!(
                "未知记忆层级: '{}'。可用: summary / events / profile（或 l1 / l2 / l3）",
                args.layer
            ))));
        }
    };
    match canonical {
        "l1" => show_l1(app, &args).await,
        "l2" => show_l2(app, &args).await,
        "l3" => show_l3(app, &args).await,
        _ => unreachable!("resolve_layer 仅返回 l1/l2/l3"),
    }
}

/// 默认查询对象：rama-0001（修复 v1.4 的 user-0001 硬编码缺陷）。
fn default_persona(args: &MemoryArgs) -> &str {
    args.persona.as_deref().unwrap_or("rama-0001")
}

// =========================================================
// L1 摘要展示
// =========================================================

async fn show_l1(app: &Arc<ramaria_app::App>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = default_persona(args);
    let memories = app
        .storage()
        .list_unabsorbed_l1(persona_uid)
        .await
        .context("查询 L1 记忆失败")?;

    if args.json {
        let items: Vec<serde_json::Value> = memories
            .iter()
            .skip(args.offset)
            .take(args.limit)
            .map(|mem| {
                serde_json::json!({
                    "id": mem.id.to_string(),
                    "session_id": mem.session_id.to_string(),
                    "summary": mem.summary,
                    "keywords": mem.keywords,
                    "atmosphere": mem.atmosphere,
                    "valence": mem.valence,
                    "salience": mem.salience,
                    "created_at": crate::util::format_timestamp_iso(mem.created_at),
                })
            })
            .collect();
        let data = serde_json::json!({
            "layer": "l1",
            "persona_uid": persona_uid,
            "total": memories.len(),
            "items": items,
        });
        return crate::json::emit_ok(&data);
    }

    if memories.is_empty() {
        crate::ui::info(&format!("{persona_uid} 暂无未吸收的 L1 记忆"));
        return Ok(());
    }

    println!();
    crate::ui::separator();
    println!(
        "  L1 记忆摘要 — {persona_uid}（{} 条）",
        memories.len().min(args.limit)
    );
    crate::ui::separator();

    for (i, mem) in memories
        .iter()
        .skip(args.offset)
        .take(args.limit)
        .enumerate()
    {
        println!();
        println!("  [{i}] {}", mem.id);
        crate::ui::labeled("会话", &mem.session_id.to_string());
        crate::ui::labeled("摘要", &crate::util::truncate(&mem.summary, 120));
        if let Some(ts) = crate::util::format_timestamp(mem.created_at) {
            crate::ui::labeled("时间", &ts);
        }
        crate::ui::labeled("效价", &format!("{:.2}", mem.valence));
        crate::ui::labeled("显著性", &format!("{:.2}", mem.salience));
    }

    if memories.len() > args.limit {
        println!();
        crate::ui::info(&format!(
            "（仅显示前 {} 条，共 {} 条）",
            args.limit,
            memories.len()
        ));
    }

    Ok(())
}

// =========================================================
// L2 事件展示
// =========================================================

async fn show_l2(app: &Arc<ramaria_app::App>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = default_persona(args);
    // 查询全量后在 CLI 层分页：保证 JSON `total` 为分页前总数（与 L1/L3 一致，
    // 供 agent 判断有无下一页；SQL LIMIT 传 i64::MAX 等价无限制，事件数不会接近该值）
    let all_events = app
        .storage()
        .list_events_by_persona(persona_uid, 0, i64::MAX)
        .await
        .context("查询 L2 事件失败")?;
    let paged: Vec<_> = all_events
        .iter()
        .skip(args.offset)
        .take(args.limit)
        .collect();

    if args.json {
        let items: Vec<serde_json::Value> = paged
            .iter()
            .map(|event| {
                serde_json::json!({
                    "id": event.id,
                    "title": event.title,
                    "summary": event.summary,
                    "keywords": event.keywords,
                    "valence": event.valence,
                    "confidence": event.confidence,
                    "salience": event.salience,
                    "start": crate::util::format_timestamp_iso(event.start),
                    "end": crate::util::format_timestamp_iso(event.end),
                })
            })
            .collect();
        let data = serde_json::json!({
            "layer": "l2",
            "persona_uid": persona_uid,
            "total": all_events.len(),
            "items": items,
        });
        return crate::json::emit_ok(&data);
    }

    if paged.is_empty() {
        crate::ui::info(&format!("{persona_uid} 暂无 L2 事件"));
        return Ok(());
    }

    println!();
    crate::ui::separator();
    println!("  L2 记忆事件 — {persona_uid}（{} 条）", paged.len());
    crate::ui::separator();

    for (i, event) in paged.iter().enumerate() {
        println!();
        println!("  [{i}] #{}", event.id);
        crate::ui::labeled("标题", &crate::util::truncate(&event.title, 80));
        crate::ui::labeled("摘要", &crate::util::truncate(&event.summary, 120));
        if let Some(ts) = crate::util::format_timestamp(event.start) {
            crate::ui::labeled("时间", &ts);
        }
        crate::ui::labeled("确凿度", &format!("{:.2}", event.confidence));
        crate::ui::labeled("显著性", &format!("{:.2}", event.salience));
    }

    Ok(())
}

// =========================================================
// L3 性格标签展示
// =========================================================

async fn show_l3(app: &Arc<ramaria_app::App>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = default_persona(args);
    let traits = app
        .storage()
        .list_traits_by_persona(persona_uid)
        .await
        .context("查询 L3 性格标签失败")?;

    if args.json {
        let items: Vec<serde_json::Value> = traits
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "trait_label": t.trait_label,
                    "meaning": t.meaning,
                    "layer": format!("{:?}", t.layer).to_lowercase(),
                    "confidence": t.confidence,
                    "evidence": t.evidence,
                    "status": format!("{:?}", t.status).to_lowercase(),
                    "created_at": crate::util::format_timestamp_iso(t.created_at),
                })
            })
            .collect();
        let data = serde_json::json!({
            "layer": "l3",
            "persona_uid": persona_uid,
            "total": traits.len(),
            "items": items,
        });
        return crate::json::emit_ok(&data);
    }

    if traits.is_empty() {
        crate::ui::info(&format!("{persona_uid} 暂无 L3 性格标签"));
        return Ok(());
    }

    // 按层分组
    let base_traits: Vec<_> = traits
        .iter()
        .filter(|t| matches!(t.layer, ramaria_core::types::TraitLayer::Base))
        .collect();
    let primary_traits: Vec<_> = traits
        .iter()
        .filter(|t| matches!(t.layer, ramaria_core::types::TraitLayer::Primary))
        .collect();
    let accent_traits: Vec<_> = traits
        .iter()
        .filter(|t| matches!(t.layer, ramaria_core::types::TraitLayer::Accent))
        .collect();

    println!();
    crate::ui::separator();
    println!("  L3 性格标签 — {persona_uid}（共 {} 条）", traits.len());
    crate::ui::separator();

    print_trait_group("底色 (Base)", &base_traits);
    print_trait_group("主色调 (Primary)", &primary_traits);
    print_trait_group("点缀 (Accent)", &accent_traits);

    // 处理未知 layer（#[non_exhaustive]）
    let others: Vec<_> = traits
        .iter()
        .filter(|t| {
            !matches!(
                t.layer,
                ramaria_core::types::TraitLayer::Base
                    | ramaria_core::types::TraitLayer::Primary
                    | ramaria_core::types::TraitLayer::Accent
            )
        })
        .collect();
    if !others.is_empty() {
        print_trait_group("其他", &others);
    }

    Ok(())
}

fn print_trait_group(label: &str, traits: &[&ramaria_core::types::PersonalityTrait]) {
    if traits.is_empty() {
        return;
    }
    println!();
    println!("  【{label}】");
    for t in traits {
        let status_mark = if t.status == ramaria_core::types::TraitStatus::Active {
            "●"
        } else {
            "○"
        };
        println!(
            "    {status_mark} {} (置信度: {:.2})",
            t.trait_label,
            t.confidence * 100.0
        );
        if !t.meaning.is_empty() {
            println!("      {}", t.meaning);
        }
    }
}

// 辅助函数已提取至 crate::util 模块：
// - crate::util::format_timestamp
// - crate::util::truncate

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 层级别名双支持：l1↔summary / l2↔events / l3↔profile。
    #[test]
    fn layer_aliases_resolve() {
        assert_eq!(resolve_layer("l1"), Some("l1"));
        assert_eq!(resolve_layer("summary"), Some("l1"));
        assert_eq!(resolve_layer("l2"), Some("l2"));
        assert_eq!(resolve_layer("events"), Some("l2"));
        assert_eq!(resolve_layer("l3"), Some("l3"));
        assert_eq!(resolve_layer("profile"), Some("l3"));
        assert_eq!(resolve_layer("l4"), None);
        assert_eq!(resolve_layer(""), None);
    }

    /// 默认 persona 为 rama-0001（修复 v1.4 的 user-0001 硬编码缺陷）。
    #[test]
    fn default_persona_is_rama() {
        let args = MemoryArgs {
            layer: "l1".to_string(),
            persona: None,
            limit: 10,
            offset: 0,
            json: false,
        };
        assert_eq!(default_persona(&args), "rama-0001");
        let args_with = MemoryArgs {
            layer: "l1".to_string(),
            persona: Some("user-0001".to_string()),
            limit: 10,
            offset: 0,
            json: false,
        };
        assert_eq!(default_persona(&args_with), "user-0001");
    }
}
