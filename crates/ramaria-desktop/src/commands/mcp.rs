//! crates/ramaria-desktop/src/commands/mcp.rs - MCP 接入面板 Tauri Commands
//!
//! 设计特点:
//! - `get_mcp_info` 只汇总面板展示所需的运行时信息（配置开关 / 库路径 / 客户端活动统计）；
//!   配置读写仍走统一写入口（`get_full_config` / `update_full_config`），不新增第二条写通道
//! - 库路径与 CLI 命令供设置页生成客户端配置片段（复制即用）；不改动 MCP 服务端行为
//! - CLI 命令探测：优先取应用同目录的 `ramaria` 可执行文件，缺失时回退 PATH 约定名
//! - 不探测 MCP 服务进程：stdio 服务由外部客户端按需拉起，桌面无法观测其进程状态，
//!   面板以"通道活动统计"呈现是否有客户端在用（活跃会话数 + 最近消息时间）
//! - 日志纪律：日志不打印绝对路径（面板展示的路径只走命令返回值，不进日志）；
//!   活动统计失败降级为 0 / None 不阻塞面板

use crate::DesktopState;
use ramaria_storage::repo::sessions as sessions_repo;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

// =========================================================
// 常量
// =========================================================

/// MCP 入口的会话通道标识（与 `ramaria-service` 的 `CHANNEL_MCP` 保持一致）。
const CHANNEL_MCP: &str = "mcp";

/// PATH 回退的 CLI 命令名（未探测到同目录可执行文件时使用）。
const CLI_COMMAND_FALLBACK: &str = "ramaria";

// =========================================================
// 前端展示用结构体
// =========================================================

/// MCP 接入信息视图（设置页「MCP 接入」面板展示与配置片段生成用）。
///
/// 序列化约定：camelCase 对齐前端 JS 访问（`info.dbPath` / `info.commandIsBundled`）。
///
/// 字段约定:
/// - `enabled`: `[mcp].enabled` 生效值（config.toml 与 DB 合并后的权威配置）。
/// - `db_path` / `config_path`: 当前数据库与配置文件绝对路径（面板展示与片段生成）。
/// - `command`: 客户端配置片段使用的命令；同目录探测到 CLI 时为绝对路径，
///   否则为 PATH 约定名 `ramaria`。
/// - `command_is_bundled`: 命令是否来自同目录探测
///   （false 表示要求安装目录在 PATH 中，面板据此提示）。
/// - `active_sessions`: `mcp` 通道未关闭会话数（0 表示当前无外部会话在写）。
/// - `last_activity_ms`: `mcp` 通道最近一条消息时间（Unix 毫秒）；无活动为 None。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInfoView {
    pub enabled: bool,
    pub db_path: String,
    pub config_path: String,
    pub command: String,
    pub command_is_bundled: bool,
    pub active_sessions: i64,
    pub last_activity_ms: Option<i64>,
}

// =========================================================
// get_mcp_info — MCP 接入信息（设置页面板）
// =========================================================

/// 汇总 MCP 接入面板所需的运行时信息。
///
/// 返回:
/// - [`McpInfoView`]：配置开关 + 库/配置路径 + CLI 命令 + 通道活动统计。
///
/// 说明:
/// - 配置读取复用 `ConfigSyncService::load_config_only`（与设置页其它区块同源的只读视图）；
/// - 活动统计查询失败按空处理（面板仍可展示与保存配置，活动信息降级为"暂无"）。
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_mcp_info(state: State<'_, DesktopState>) -> Result<McpInfoView, String> {
    // ---- 配置（只读，无写副作用） ----
    let config_sync =
        ramaria_app::ConfigSyncService::new(state.app.storage().clone(), state.config_path.clone());
    let config = config_sync
        .load_config_only()
        .await
        .map_err(|e| format!("读取配置失败: {}", e))?;

    // ---- 通道活动统计（降级不阻塞） ----
    let overview = match sessions_repo::channel_overview(&state.pool, CHANNEL_MCP).await {
        Ok(overview) => overview,
        Err(e) => {
            tracing::warn!(error = %e, "统计 MCP 通道会话概览失败，活动信息降级为默认值");
            sessions_repo::ChannelOverview {
                active_sessions: 0,
                last_activity_ms: None,
            }
        }
    };

    // ---- CLI 命令解析（同目录探测 → PATH 约定名） ----
    let (command, command_is_bundled) = resolve_cli_command();

    tracing::debug!(
        enabled = config.mcp.enabled,
        active_sessions = overview.active_sessions,
        command_is_bundled,
        "get_mcp_info 完成"
    );

    Ok(McpInfoView {
        enabled: config.mcp.enabled,
        db_path: state.db_path.to_string_lossy().to_string(),
        config_path: state.config_path.to_string_lossy().to_string(),
        command,
        command_is_bundled,
        active_sessions: overview.active_sessions,
        last_activity_ms: overview.last_activity_ms,
    })
}

// =========================================================
// CLI 命令探测
// =========================================================

/// 在指定目录中探测 Ramaria CLI 可执行文件。
///
/// 参数:
/// - `dir`: 待探测目录（通常为桌面可执行文件所在目录）。
///
/// 返回:
/// - `Some(path)`: 目录内存在 `ramaria.exe`（Windows）或 `ramaria`（其他平台）。
/// - `None`: 未探测到（调用方回退 PATH 约定名）。
fn detect_cli_in_dir(dir: &Path) -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &["ramaria.exe", "ramaria"]
    } else {
        &["ramaria"]
    };
    candidates
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// 解析生成客户端配置片段使用的 CLI 命令。
///
/// 优先级:
/// 1. 当前可执行文件同目录的 `ramaria` 可执行文件（发行包与开发构建的常见布局）；
/// 2. 回退约定命令名 `ramaria`（要求安装目录在 PATH 中）。
///
/// 返回:
/// - `(命令字符串, 是否来自同目录探测)`：命令字符串直接进客户端配置片段的 `command` 字段。
fn resolve_cli_command() -> (String, bool) {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && let Some(found) = detect_cli_in_dir(dir)
    {
        return (found.to_string_lossy().to_string(), true);
    }
    (CLI_COMMAND_FALLBACK.to_string(), false)
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 同目录探测：空目录不命中；写入可执行文件后命中预期路径。
    #[test]
    fn detect_cli_in_dir_prefers_bundled_binary() {
        let dir =
            std::env::temp_dir().join(format!("ramaria-desktop-mcp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("创建测试临时目录失败");

        // 空目录：无 CLI 可执行文件
        assert!(
            detect_cli_in_dir(&dir).is_none(),
            "空目录不应探测到 CLI 可执行文件"
        );

        // 放入平台对应的可执行文件名 → 命中该路径
        let name = if cfg!(windows) {
            "ramaria.exe"
        } else {
            "ramaria"
        };
        std::fs::write(dir.join(name), b"stub").expect("写入桩文件失败");
        let found = detect_cli_in_dir(&dir).expect("应探测到 CLI 可执行文件");
        assert_eq!(found, dir.join(name));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
