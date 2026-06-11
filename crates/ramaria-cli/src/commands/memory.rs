//! rust/crates/ramaria-cli/src/commands/memory.rs - 记忆查看命令
//!
//! 设计特点:
//! - 支持 L1（摘要）/ L2（事件）/ L3（性格）三层记忆查看
//! - 默认显示 L1，--layer 指定层级
//! - --persona 筛选特定 persona 的记忆
//! - --limit 控制输出条数
//! - 表格化展示关键字段

use anyhow::Context;
use std::sync::Arc;

/// memory 命令参数。
pub struct MemoryArgs {
    /// 记忆层级: l1 / l2 / l3
    pub layer: String,
    /// 按 persona_uid 筛选
    pub persona: Option<String>,
    /// 输出条数上限
    pub limit: usize,
}

/// 执行 memory 命令。
pub async fn run(app: &Arc<ramaria_app::App>, args: MemoryArgs) -> anyhow::Result<()> {
    match args.layer.as_str() {
        "l1" => show_l1(app, &args).await,
        "l2" => show_l2(app, &args).await,
        "l3" => show_l3(app, &args).await,
        other => {
            anyhow::bail!("未知记忆层级: '{other}'。支持: l1 / l2 / l3");
        }
    }
}

// =========================================================
// L1 摘要展示
// =========================================================

async fn show_l1(app: &Arc<ramaria_app::App>, args: &MemoryArgs) -> anyhow::Result<()> {
    let persona_uid = args.persona.as_deref().unwrap_or("user-0001");
    let memories = app
        .storage()
        .list_unabsorbed_l1(persona_uid)
        .await
        .context("查询 L1 记忆失败")?;

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

    for (i, mem) in memories.iter().take(args.limit).enumerate() {
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
    let persona_uid = args.persona.as_deref().unwrap_or("user-0001");
    let events = app
        .storage()
        .list_events_by_persona(persona_uid, 0, args.limit as i64)
        .await
        .context("查询 L2 事件失败")?;

    if events.is_empty() {
        crate::ui::info(&format!("{persona_uid} 暂无 L2 事件"));
        return Ok(());
    }

    println!();
    crate::ui::separator();
    println!("  L2 记忆事件 — {persona_uid}（{} 条）", events.len());
    crate::ui::separator();

    for (i, event) in events.iter().enumerate() {
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
    let persona_uid = args.persona.as_deref().unwrap_or("user-0001");
    let traits = app
        .storage()
        .list_traits_by_persona(persona_uid)
        .await
        .context("查询 L3 性格标签失败")?;

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
//   - crate::util::format_timestamp()
//   - crate::util::truncate()
