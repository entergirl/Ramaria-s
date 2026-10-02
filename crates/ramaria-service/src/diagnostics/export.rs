//! crates/ramaria-service/src/diagnostics/export.rs - 诊断导出用例入口与请求 / 结果类型
//!
//! 设计特点:
//! - 一站式编排：系统信息 / 日志 / 配置 / 检索索引构建状态采集 → 打包 zip → 原子替换
//! - 收集与打包全程在配置快照锁外进行（锁内只做克隆）
//! - 收集阶段错误不阻塞导出：缺失项记录占位文本与状态，而非报错退出
//! - 报告携带各步骤状态（含 `index_build` 三态与 zip 结果），供宿主展示与失败诊断
//! - 输出路径安全性由调用方保证（本层不做路径白名单校验）

use std::collections::HashMap;
use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::engine::Engine;

use super::collect::{
    collect_config, collect_index_build_status, collect_logs, collect_system_info,
};
use super::redact::path_log_label;
use super::render::build_zip;

// =========================================================
// 类型定义
// =========================================================

/// 诊断导出请求。
///
/// 字段约定:
/// - `output_path`: 输出 .zip 文件的绝对路径（由调用方通过文件对话框或 CLI 参数指定）。
/// - `schema_version`: 数据库 schema 版本号字符串（来自存储层 `schema_meta`）。
#[derive(Debug, Clone)]
pub struct DiagnosticsRequest {
    /// 输出的 .zip 文件绝对路径
    pub output_path: PathBuf,
    /// 数据库 schema 版本号字符串
    pub schema_version: String,
}

/// 诊断导出结果。
///
/// 包含导出文件的绝对路径和各组件的成功/失败状态。
#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    /// 输出的 .zip 文件绝对路径
    pub output_path: PathBuf,
    /// 各收集步骤的状态
    pub collection_status: HashMap<String, String>,
    /// 文件大小（字节）
    pub file_size_bytes: u64,
}

// =========================================================
// 公开 API
// =========================================================

/// 导出诊断信息，打包为 .zip 文件。
///
/// 用法:
/// - 入口层在用户手动导出诊断信息时调用（桌面设置页 / CLI 子命令）。
///
/// 参数:
/// - `engine`: 服务层引擎（配置快照提供日志目录与配置目录）。
/// - `req`: 导出请求（输出路径 + 数据库 schema 版本号）。
///
/// 返回:
/// - `DiagnosticsReport`，含输出路径、各步骤状态和文件大小。
///
/// 导出内容:
/// - `ramaria.log`: 最近最多 1000 行日志内容（从日志文件读取）。
/// - `config.toml`: 当前配置文件内容（API key 已脱敏为 `[REDACTED]`）。
/// - `system.txt`: OS / 架构 / 版本 / schema 版本 / 采集时间。
///
/// 报告字段:
/// - `collection_status` 含各收集步骤状态；其中 `index_build` 取值为
///   `ok` / `failed: <脱敏原因>` / `not_built`（检索索引构建状态，供失败诊断）。
///
/// 安全约束:
/// - API key 在收集阶段即脱敏，写入前已不可逆。
/// - 日志与配置在打包前经 [`redact_for_export`](crate::diagnostics::redact_for_export)
///   二次脱敏：绝对路径只留文件名，消息类字段值只留字符数（不落原文）。
/// - 输出路径的安全性由调用方保证（本层不做路径白名单校验）。
///
/// 示例:
/// ```ignore
/// let report = engine.export_diagnostics(DiagnosticsRequest {
///     output_path: PathBuf::from("C:/Users/me/Desktop/ramaria-diagnostics.zip"),
///     schema_version: "1".to_string(),
/// }).await?;
/// ```
pub(crate) async fn export(
    engine: &Engine,
    req: DiagnosticsRequest,
) -> RamariaResult<DiagnosticsReport> {
    // 配置快照在锁内克隆后释放锁，收集与打包全程在锁外进行
    let config = engine.config();
    let mut status = HashMap::new();

    // 1. 收集系统信息（纯内存操作，不会失败）
    let system_info = collect_system_info(&req.schema_version);

    // 2. 收集日志（最近 1000 行）
    let logs = collect_logs(config.as_ref(), &mut status);

    // 3. 收集配置（API key 脱敏）
    let config_content = collect_config(config.as_ref(), &mut status);

    // 4. 收集检索索引构建状态（最近失败原因 / 已构建 / 未构建）
    collect_index_build_status(engine, &mut status);

    // 5. 打包为 zip
    let output_path = req.output_path;
    let file_size = build_zip(&output_path, &system_info, &logs, &config_content)
        .map_err(|e| RamariaError::io(format!("生成诊断 zip 文件失败: {e}"), None))?;

    status.insert("zip".to_string(), "ok".to_string());

    tracing::info!(
        file = %path_log_label(&output_path),
        size = file_size,
        "诊断信息导出完成"
    );

    Ok(DiagnosticsReport {
        output_path,
        collection_status: status,
        file_size_bytes: file_size,
    })
}
