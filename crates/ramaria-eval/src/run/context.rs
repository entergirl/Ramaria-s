//! crates/ramaria-eval/src/run/context.rs - Ramaria 评测全量上下文基线
//!
//! 设计特点:
//! - 只读：不写库、不封存、不触碰检索器，条件间互不污染
//! - 分页读取 L1 摘要，按发生时间升序渲染，与对话阅读顺序一致
//! - 超预算从最早开始丢弃、保留最近，至少留一条交给后续 token 预算兜底
//! - 空 L1 返回固定占位行，调用方无需区分 `None` 与空串
//! - 这是 full-context 基线的唯一实现，不复制召回渲染逻辑

use ramaria_core::error::RamariaResult;
use ramaria_service::{Engine, L1BrowseRequest, L1MemoryView};

#[cfg(test)]
mod tests;

/// 单页读取条数（服务层直查口径的上限）。
const PAGE_SIZE: u32 = 1000;

/// 渲染头。
const HEADER: &str = "[相关记忆]";

/// 空 L1 的占位正文。
const EMPTY_BODY: &str = "（无）";

// =========================================================
// 上下文装配
// =========================================================

/// 拼装某人格的全量记忆上下文。
///
/// 参数:
/// - `engine`: 条件引擎（只读使用）；
/// - `persona_uid`: 目标人格；
/// - `budget_chars`: 字符预算（含头部与编号）。
///
/// 返回:
/// - 成功时返回渲染文本；读取失败返回存储错误。
///
/// 说明:
/// - 条目按 `occurred_at` 升序，同值再按 `created_at` 升序；
/// - 超预算时从最早条目开始整条丢弃，至少保留最近一条；
/// - 单条自身超预算也保留，token 预算由生成侧兜底。
pub async fn build_full_context(
    engine: &Engine,
    persona_uid: &str,
    budget_chars: usize,
) -> RamariaResult<String> {
    let mut views = collect_l1(engine, persona_uid).await?;
    if views.is_empty() {
        return Ok(format!("{HEADER}\n{EMPTY_BODY}"));
    }

    views.sort_by_key(|view| (view.occurred_at, view.created_at));
    Ok(render(&views, budget_chars))
}

/// 分页取回某人格的全部 L1 摘要。
async fn collect_l1(engine: &Engine, persona_uid: &str) -> RamariaResult<Vec<L1MemoryView>> {
    let mut views = Vec::new();
    let mut offset = 0u32;
    loop {
        let page = engine
            .memory_l1(L1BrowseRequest {
                persona: Some(persona_uid.to_string()),
                unabsorbed_only: false,
                limit: Some(PAGE_SIZE),
                offset: Some(offset),
            })
            .await?;
        let received = page.items.len();
        views.extend(page.items);
        if received < PAGE_SIZE as usize {
            return Ok(views);
        }
        offset = offset.saturating_add(PAGE_SIZE);
    }
}

// =========================================================
// 渲染
// =========================================================

/// 按预算裁剪后渲染为固定文本块。
fn render(views: &[L1MemoryView], budget_chars: usize) -> String {
    let lines: Vec<String> = views
        .iter()
        .enumerate()
        .map(|(index, view)| format!("{}. (L1) {}", index + 1, view.summary.trim()))
        .collect();

    let mut total = HEADER.chars().count()
        + 1
        + lines
            .iter()
            .map(|line| line.chars().count() + 1)
            .sum::<usize>();

    // 从最早开始整条丢弃；至少保留一条，编号在裁剪后重排。
    let mut start = 0usize;
    while start + 1 < lines.len() && total > budget_chars {
        total -= lines[start].chars().count() + 1;
        start += 1;
    }

    let mut output = String::from(HEADER);
    for (index, view) in views[start..].iter().enumerate() {
        output.push('\n');
        output.push_str(&format!("{}. (L1) {}", index + 1, view.summary.trim()));
    }
    output
}
