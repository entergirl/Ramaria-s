//! crates/ramaria-cli/src/commands/index_cmd.rs - 索引管理命令
//!
//! 设计特点:
//! - rebuild: 从存储层重建内存检索器索引（BM25 + 向量 + 图谱）
//! - 显示重建进度（文档计数）与当前应用状态
//! - `--json` 输出信封: `data` 为 `{"doc_count","state","elapsed_ms"}`
//! - 记录 tracing 日志用于诊断

use anyhow::Context;
use std::sync::Arc;

/// 重建索引。
///
/// 参数:
/// - `json`: `--json` 信封输出（stdout 仅一行结构化数据）。
pub async fn run(app: &Arc<ramaria_app::App>, json: bool) -> anyhow::Result<()> {
    crate::ui::info("正在重建检索索引...");
    crate::ui::info("这可能需要一些时间，取决于数据量大小。");

    let start = std::time::Instant::now();
    // 保留 RamariaError source：main 按错误链映射退出码（不可用时为 3）
    let count = app.rebuild_retriever().await.context("索引重建失败")?;
    let elapsed = start.elapsed();

    if json {
        return crate::json::emit_ok(&serde_json::json!({
            "doc_count": count,
            "state": app.current_state().as_str(),
            "elapsed_ms": elapsed.as_millis() as u64,
        }));
    }

    crate::ui::success(&format!(
        "索引重建完成 — {count} 篇文档，耗时 {:.1}s",
        elapsed.as_secs_f64()
    ));

    // 显示当前状态
    let state = app.current_state();
    crate::ui::info(&format!("当前应用状态: {}", state.as_str()));

    Ok(())
}
