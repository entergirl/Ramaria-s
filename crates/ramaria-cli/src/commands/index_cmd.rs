//! crates/ramaria-cli/src/commands/index_cmd.rs - 索引管理命令
//!
//! 设计特点:
//! - rebuild: 从存储层重建内存检索器索引（BM25 + 向量 + 图谱）
//! - 显示重建进度（文档计数）
//! - 错误不中断，继续处理剩余文档
//! - 记录 tracing 日志用于诊断

use std::sync::Arc;

/// 重建索引。
pub async fn run(app: &Arc<ramaria_app::App>) -> anyhow::Result<()> {
    crate::ui::info("正在重建检索索引...");
    crate::ui::info("这可能需要一些时间，取决于数据量大小。");

    let start = std::time::Instant::now();

    match app.rebuild_retriever().await {
        Ok(count) => {
            let elapsed = start.elapsed();
            crate::ui::success(&format!(
                "索引重建完成 — {count} 篇文档，耗时 {:.1}s",
                elapsed.as_secs_f64()
            ));
        }
        Err(e) => {
            crate::ui::print_error(&e);
            anyhow::bail!("索引重建失败");
        }
    }

    // 显示当前状态
    let state = app.current_state();
    crate::ui::info(&format!("当前应用状态: {}", state.as_str()));

    Ok(())
}
