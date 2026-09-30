//! crates/ramaria-cli/src/commands/mcp.rs - MCP 服务端命令（ramaria mcp serve）
//!
//! 设计特点:
//! - 薄壳：只把全局 `--db` 透传给 `ramaria-mcp` 宿主，不含任何协议或业务逻辑
//! - 不构造应用层实例：MCP 走与传输无关的服务层（避免重复连接池与宿主侧副作用）
//! - stdout 纪律：协议消息由 stdio 传输独占，本命令不向 stdout 写任何内容
//! - 生命周期：阻塞至客户端关闭连接或收到 Ctrl+C（宿主内部处理）

use std::path::PathBuf;

use anyhow::Context;

/// 以 stdio 启动 MCP 服务端。
///
/// 参数:
/// - `db_path`: 数据库文件路径（来自全局 `--db` / `RAMARIA_DB_PATH`）。
///
/// 返回:
/// - 成功时返回服务端退出后的结果；装配或握手失败时返回带上下文的错误。
pub async fn serve(db_path: PathBuf) -> anyhow::Result<()> {
    let options = ramaria_mcp::McpHostOptions::new(db_path);
    ramaria_mcp::serve_stdio(options)
        .await
        .context("MCP 服务端运行失败")?;
    Ok(())
}
