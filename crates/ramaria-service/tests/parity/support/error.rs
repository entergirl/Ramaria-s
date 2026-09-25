//! crates/ramaria-service/tests/parity/support/error.rs - 对照测试基建错误类型
//!
//! 设计特点:
//! - 统一基建返回类型 `ParityResult<T>`：环境构建 / 造数 / 快照读写失败均返回结构化错误
//! - 错误携带上下文：环境失败带用例标签，文件失败带路径，用例失败带用例名与来源错误
//! - 保留来源错误链（用例返回的 `RamariaError` 转字符串附在消息后），便于排查
//! - 断言类失败（对照不等价 / 基线不一致）不走本类型：直接 panic 并输出差异报告
//! - 仅测试基建使用，不进入生产代码路径

use std::fmt;
use std::path::PathBuf;

/// 对照测试基建的统一错误。
///
/// 职责:
/// - 表达"环境无法构建 / 造数失败 / 基线文件读写异常"三类基建问题；
/// - 说明:
/// - 断言失败（行为不等价）不属于本类型：测试直接 panic 并输出差异报告，避免被
///   `?` 静默吞掉。
#[derive(Debug)]
pub enum ParityError {
    /// 环境构建或造数失败（临时库 / 引擎装配 / 存储写入）。
    Env {
        /// 出错的步骤或用例标签。
        context: String,
        /// 可读原因（含来源错误链）。
        message: String,
    },
    /// 基线文件读写失败。
    Golden {
        /// 出错的文件路径（为空表示目录级操作）。
        path: PathBuf,
        /// 可读原因。
        message: String,
    },
}

impl ParityError {
    /// 构造环境类错误。
    ///
    /// 用法:
    /// - 用例代码把 `RamariaError` 转成本类型时附上步骤说明，
    ///   例如 `ParityError::env("封存", e)`。
    ///
    /// 参数:
    /// - `context`: 出错步骤（如"种子写入 persona"）。
    /// - `source`: 来源错误（任意可展示错误）。
    pub fn env(context: impl Into<String>, source: impl fmt::Display) -> Self {
        Self::Env {
            context: context.into(),
            message: source.to_string(),
        }
    }

    /// 构造基线文件错误。
    ///
    /// 参数:
    /// - `path`: 出错文件路径。
    /// - `source`: 来源错误。
    pub fn golden(path: impl Into<PathBuf>, source: impl fmt::Display) -> Self {
        Self::Golden {
            path: path.into(),
            message: source.to_string(),
        }
    }
}

impl fmt::Display for ParityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env { context, message } => {
                write!(f, "对照测试环境失败（{context}）：{message}")
            }
            Self::Golden { path, message } => {
                write!(f, "对照基线文件失败（{}）：{message}", path.display())
            }
        }
    }
}

impl std::error::Error for ParityError {}

/// 基建返回类型别名。
pub type ParityResult<T> = Result<T, ParityError>;
