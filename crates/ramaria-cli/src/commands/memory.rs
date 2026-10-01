//! crates/ramaria-cli/src/commands/memory.rs - 记忆查看命令
//!
//! 设计特点:
//! - 支持 L1（摘要）/ L2（事件）/ L3（性格）三层记忆查看
//! - 层级别名双支持: l1↔summary / l2↔events / l3↔profile，纠错提示同时列出
//! - 默认显示 L1，默认 persona 为 rama-0001
//! - --persona 筛选特定 persona 的记忆
//! - --limit/--offset 控制分页（分页与总数由服务层用例给出）
//! - --json 输出信封（时间戳统一 ISO-8601 UTC；L2 条目由服务层视图转换），文本模式表格化展示

use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_service::{Engine, L1BrowseRequest, L2BrowseRequest, L2EventView};
use serde::Serialize;
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
pub async fn run(engine: &Arc<Engine>, args: MemoryArgs) -> anyhow::Result<()> {
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
        "l1" => show_l1(engine, &args).await,
        "l2" => show_l2(engine, &args).await,
        "l3" => show_l3(engine, &args).await,
        _ => unreachable!("resolve_layer 仅返回 l1/l2/l3"),
    }
}

/// 默认查询对象：rama-0001。
fn default_persona(args: &MemoryArgs) -> &str {
    args.persona.as_deref().unwrap_or("rama-0001")
}

// =========================================================
// L1 摘要展示
// =========================================================

async fn show_l1(engine: &Arc<Engine>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = default_persona(args);
    // 未吸收口径（与既有 CLI 展示一致）
    let page = engine
        .memory_l1(L1BrowseRequest {
            persona: Some(persona_uid.to_string()),
            unabsorbed_only: true,
            limit: Some(args.limit as u32),
            offset: Some(args.offset as u32),
        })
        .await
        .context("查询 L1 记忆失败")?;

    if args.json {
        let items: Vec<serde_json::Value> = page
            .items
            .iter()
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
            "total": page.total,
            "items": items,
        });
        return crate::json::emit_ok(&data);
    }

    if page.total == 0 {
        crate::ui::info(&format!("{persona_uid} 暂无未吸收的 L1 记忆"));
        return Ok(());
    }

    println!();
    crate::ui::separator();
    println!(
        "  L1 记忆摘要 — {persona_uid}（{} 条）",
        page.total.min(args.limit)
    );
    crate::ui::separator();

    for (i, mem) in page.items.iter().enumerate() {
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

    if page.total > args.limit {
        println!();
        crate::ui::info(&format!(
            "（仅显示前 {} 条，共 {} 条）",
            args.limit, page.total
        ));
    }

    Ok(())
}

// =========================================================
// L2 事件展示
// =========================================================

/// `memory l2` 的 `--json` 数据负载（信封内 `data` 字段）。
///
/// 字段约定:
/// - `items`: 事件条目（由 [`l2_json_items`] 从服务层视图转换，时间戳为 ISO-8601 UTC）。
#[derive(Debug, Serialize)]
struct L2JsonData<'a> {
    layer: &'static str,
    persona_uid: &'a str,
    total: usize,
    items: Vec<serde_json::Value>,
}

async fn show_l2(engine: &Arc<Engine>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = default_persona(args);
    // 分页下沉服务层视图：`total` 为该人格分页前的全量计数（与 L1 / L3 口径一致，
    // 供 agent 判断有无下一页），展示列所需的起止时间由视图透出。
    let page = engine
        .memory_l2(L2BrowseRequest {
            persona: Some(persona_uid.to_string()),
            limit: Some(args.limit as u32),
            offset: Some(offset_as_u32(args.offset)),
        })
        .await
        .context("查询 L2 事件失败")?;

    if args.json {
        let data = L2JsonData {
            layer: "l2",
            persona_uid,
            total: page.total,
            items: l2_json_items(&page.items),
        };
        return crate::json::emit_ok(&data);
    }

    if page.items.is_empty() {
        crate::ui::info(&format!("{persona_uid} 暂无 L2 事件"));
        return Ok(());
    }

    print!("{}", l2_text_block(persona_uid, &page.items));
    Ok(())
}

/// 渲染 L2 事件的 `--json` 条目（服务层视图 → 表示层，仅做时间换算）。
///
/// 参数:
/// - `items`: 当前页事件视图（顺序即输出顺序）。
///
/// 返回:
/// - 每条条目为 `{id, title, summary, keywords, valence, confidence, salience, start, end}`；
///   `start` / `end` 为 ISO-8601 UTC 字符串，非正值输出 `null`。
///
/// 说明:
/// - 条目字段集为 `--json` 对外契约（视图字段扩展不改变条目字段集），
///   `persona_uid` / `created_at` / `presentation` 等视图字段不进入条目。
pub fn l2_json_items(items: &[L2EventView]) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|item| {
            serde_json::json!({
                "id": item.id,
                "title": item.title,
                "summary": item.summary,
                "keywords": item.keywords,
                "valence": item.valence,
                "confidence": item.confidence,
                "salience": item.salience,
                "start": crate::util::format_timestamp_iso(item.start),
                "end": crate::util::format_timestamp_iso(item.end),
            })
        })
        .collect()
}

/// 分页偏移转服务层口径（超出 u32 上限时收敛到上限，等价于"已无更多数据"）。
fn offset_as_u32(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// 渲染 L2 事件文本块（编号 / id / 标题 / 摘要 / 时间 / 确凿度 / 显著性）。
///
/// 参数:
/// - `persona_uid`: 标题行展示的归属人格。
/// - `items`: 当前页的事件视图（顺序即展示顺序；调用方保证非空）。
///
/// 返回:
/// - 完整文本块（含首行空行与分隔线），与逐行输出等价。
///
/// 说明:
/// - 时间列取事件的 `start`（`created_at` 为写入时间，不用于展示）。
fn l2_text_block(persona_uid: &str, items: &[L2EventView]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(items.len() * 7 + 4);
    lines.push(String::new());
    lines.push(crate::ui::separator_line());
    lines.push(format!(
        "  L2 记忆事件 — {persona_uid}（{} 条）",
        items.len()
    ));
    lines.push(crate::ui::separator_line());

    for (i, item) in items.iter().enumerate() {
        lines.push(String::new());
        lines.push(format!("  [{i}] #{}", item.id));
        lines.push(crate::ui::labeled_line(
            "标题",
            &crate::util::truncate(&item.title, 80),
        ));
        lines.push(crate::ui::labeled_line(
            "摘要",
            &crate::util::truncate(&item.summary, 120),
        ));
        if let Some(ts) = crate::util::format_timestamp(item.start) {
            lines.push(crate::ui::labeled_line("时间", &ts));
        }
        lines.push(crate::ui::labeled_line(
            "确凿度",
            &format!("{:.2}", item.confidence),
        ));
        lines.push(crate::ui::labeled_line(
            "显著性",
            &format!("{:.2}", item.salience),
        ));
    }

    let mut block = lines.join("\n");
    block.push('\n');
    block
}

// =========================================================
// L3 性格标签展示
// =========================================================

async fn show_l3(engine: &Arc<Engine>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = default_persona(args);
    let traits = engine
        .memory_l3(Some(persona_uid))
        .await
        .context("查询 L3 性格标签失败")?;

    if args.json {
        let items: Vec<serde_json::Value> = traits
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "trait_label": t.label,
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

fn print_trait_group(label: &str, traits: &[&ramaria_service::L3TraitView]) {
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
            t.label,
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
mod tests;
