//! crates/ramaria-eval/src/oracle/mod.rs - Ramaria 评测压缩层 oracle 入口
//!
//! 设计特点:
//! - 复用被引用运行的冻结矩阵：只把管线轴换成 oracle 预置上下文，档位 / 预算 / 模型不动
//! - 运行根固定落在被引用运行的 oracle 子目录，可续跑，也可用 --output 覆盖
//! - 产物与普通运行同构（run.json / items.jsonl / 断点），判分无需区分来源
//! - 只做路径解析与矩阵改写，条件循环全部交给共用执行器
//! - oracle 管线不进矩阵文件：读写都只经本入口

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::run::{
    Pipeline, RUN_FILE, RunControl, RunMatrix, RunReport, RunState, execute, load_tiers,
};

#[cfg(test)]
mod tests;

/// 相对被引用运行根的子目录名。
const ORACLE_DIR: &str = "oracle";

// =========================================================
// 入口
// =========================================================

/// 以预置上下文运行压缩层 oracle。
///
/// 参数:
/// - `run_id`: 被引用的运行 id（也可直接给运行根目录路径）；
/// - `output`: 输出根目录，缺省为被引用运行根下的 oracle 子目录。
///
/// 返回:
/// - 成功时返回运行摘要（产物同构于普通运行）；
/// - 运行状态缺失 / 不可解析、档位产物缺失返回结构化错误。
///
/// 说明:
/// - 条件只跑一次预置上下文：gold 覆盖到的 L1 摘要按时间升序注入，召回被跳过；
/// - 运行根已存在时按续跑处理，已完成题跳过。
pub async fn run(run_id: &str, output: Option<PathBuf>) -> RamariaResult<RunReport> {
    let cwd = current_dir()?;
    let base = crate::score::resolve_run_root(&cwd, run_id);
    let state = read_state(&base)?;
    let matrix = freeze(state.matrix);
    let tiers = load_tiers(&cwd, &matrix, None)?;
    let root = output.unwrap_or_else(|| base.join(ORACLE_DIR));
    execute(matrix, root, tiers, RunControl::default()).await
}

/// 冻结矩阵：管线轴换成 oracle 预置上下文，其余轴保持不变。
fn freeze(mut matrix: RunMatrix) -> RunMatrix {
    matrix.run.pipelines = vec![Pipeline::OracleL1];
    matrix
}

/// 读取被引用运行的冻结矩阵。
fn read_state(root: &Path) -> RamariaResult<RunState> {
    let path = root.join(RUN_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(
            format!("读取运行状态失败: {}（请确认运行 id）", path.display()),
            Some(error),
        )
    })?;
    serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!("解析运行状态失败: {}: {error}", path.display()))
    })
}

/// 读取当前工作目录。
fn current_dir() -> RamariaResult<PathBuf> {
    std::env::current_dir().map_err(|error| RamariaError::io("读取当前工作目录失败", Some(error)))
}
