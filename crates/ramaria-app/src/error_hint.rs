//! rust/crates/ramaria-app/src/error_hint.rs - 错误到 UI/CLI 提示映射
//!
//! 设计特点:
//! - 将 `RamariaError::category()` 映射为面向最终用户的友好提示
//! - 每条提示包含: 简短摘要（title）+ 详细建议（detail）
//! - 支持可重试标记（retryable），供 UI 决定是否显示"重试"按钮
//! - 未识别类别保守提示"查看日志"，不泄露内部错误细节
//!
//! 安全约束:
//! - 不暴露 API key、完整路径或数据库内部信息
//! - `detail` 中不包含 raw error context（避免向用户泄露堆栈/密钥）

use ramaria_core::error::RamariaError;

// =========================================================
// ErrorHint 结构体
// =========================================================

/// 面向用户的错误提示。
///
/// 职责:
/// - 将内部 `RamariaError` 翻译为用户可理解的简短描述和操作建议。
/// - `retryable` 标记指示 UI 是否应显示"重试"按钮。
///
/// 字段约定:
/// - `title`: 一行内展示的错误分类摘要。
/// - `detail`: 多行建议文本（换行符分隔），UI 可逐行展示。
/// - `retryable`: 用户是否可通过重试解决（如网络错误），false 表示需改变配置或重启。
#[derive(Debug, Clone)]
pub struct ErrorHint {
    /// 错误分类标题（一行）
    pub title: String,
    /// 详细建议（可含换行）
    pub detail: String,
    /// 是否可通过重试解决
    pub retryable: bool,
}

impl ErrorHint {
    /// 从 `RamariaError` 生成用户提示。
    ///
    /// 参数:
    /// - `err`: app 层捕获的统一错误。
    ///
    /// 返回:
    /// - 面向用户的 `ErrorHint`，绝不 panic。
    ///
    /// 映射规则:
    /// - `config`: 配置错误，需检查设置 → 不可重试
    /// - `storage`: 数据库错误，需检查磁盘/权限 → 不可重试
    /// - `llm`: LLM 服务错误，通常可重试 → 可重试
    /// - `privacy`: 隐私/密钥错误，需完成设置 → 不可重试
    /// - `index`: 索引错误，需重建 → 不可重试
    /// - `validation`: 输入校验错误，需修正输入 → 不可重试
    /// - `io`: 文件 I/O 错误，需检查磁盘/权限 → 不可重试
    /// - `unsupported`: 功能不可用，需升级版本 → 不可重试
    pub fn from_error(err: &RamariaError) -> Self {
        match err.category() {
            "config" => Self {
                title: "配置错误".to_string(),
                detail: concat!(
                    "应用配置存在问题。\n",
                    "建议：请重新运行设置向导，或检查 config.toml 文件是否正确。\n",
                    "如果问题持续，请尝试删除配置文件后重新设置。"
                )
                .to_string(),
                retryable: false,
            },

            "storage" => Self {
                title: "数据库错误".to_string(),
                detail: concat!(
                    "数据库读写失败，可能是磁盘空间不足或数据目录权限问题。\n",
                    "建议：检查数据目录是否存在且可读写；尝试重启应用。\n",
                    "如果问题持续，可能需要重建数据库（数据将丢失）。"
                )
                .to_string(),
                retryable: false,
            },

            "llm" => Self {
                title: "LLM 服务错误".to_string(),
                detail: concat!(
                    "语言模型服务暂时不可用。\n",
                    "可能原因：网络连接中断、服务端过载、API key 无效。\n",
                    "建议：检查网络连接；确认 LLM 服务正在运行；稍后重试。"
                )
                .to_string(),
                retryable: true,
            },

            "privacy" => Self {
                title: "隐私设置未完成".to_string(),
                detail: concat!(
                    "使用线上 LLM 服务前需要完成隐私确认。\n",
                    "建议：请进入设置页面，完成隐私确认并为线上服务配置 API key。"
                )
                .to_string(),
                retryable: false,
            },

            "index" => Self {
                title: "索引错误".to_string(),
                detail: concat!(
                    "记忆索引出现问题。\n",
                    "建议：尝试手动重建索引（设置 → 索引管理 → 重建索引）。\n",
                    "如果问题持续，请重启应用。"
                )
                .to_string(),
                retryable: false,
            },

            "validation" => Self {
                title: "输入格式错误".to_string(),
                detail: concat!(
                    "输入数据不符合要求。\n",
                    "建议：检查输入内容，移除特殊字符后重试。"
                )
                .to_string(),
                retryable: false,
            },

            "io" => Self {
                title: "文件读写错误".to_string(),
                detail: concat!(
                    "读取或写入文件时出错。\n",
                    "建议：检查磁盘空间是否充足；确认应用有文件读写权限。\n",
                    "如果问题持续，请尝试以管理员身份运行。"
                )
                .to_string(),
                retryable: false,
            },

            "unsupported" => Self {
                title: "功能不可用".to_string(),
                detail: concat!(
                    "当前版本不支持此功能。\n",
                    "建议：请升级到最新版本，或等待后续更新。"
                )
                .to_string(),
                retryable: false,
            },

            _ => Self {
                title: "未知错误".to_string(),
                detail: concat!(
                    "发生了未预期的错误。\n",
                    "建议：请查看应用日志获取详细信息；尝试重启应用。"
                )
                .to_string(),
                retryable: true,
            },
        }
    }
}

// =========================================================
// 便捷函数
// =========================================================

/// 从 `RamariaError` 快速获取用户提示标题。
///
/// 用法:
/// - CLI 直接打印标题。
/// - Desktop 在通知栏显示标题。
pub fn error_title(err: &RamariaError) -> String {
    ErrorHint::from_error(err).title
}

/// 从 `RamariaError` 快速获取用户提示详情。
pub fn error_detail(err: &RamariaError) -> String {
    ErrorHint::from_error(err).detail
}

/// 判断错误是否可重试。
pub fn is_retryable(err: &RamariaError) -> bool {
    ErrorHint::from_error(err).retryable
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_not_retryable() {
        let err = RamariaError::config("配置缺失");
        let hint = ErrorHint::from_error(&err);
        assert_eq!(hint.title, "配置错误");
        assert!(!hint.retryable);
        assert!(hint.detail.contains("设置向导"));
    }

    #[test]
    fn llm_error_retryable() {
        let err = RamariaError::llm("连接超时");
        let hint = ErrorHint::from_error(&err);
        assert_eq!(hint.title, "LLM 服务错误");
        assert!(hint.retryable);
    }

    #[test]
    fn storage_error_not_retryable() {
        let err = RamariaError::storage("数据库损坏");
        let hint = ErrorHint::from_error(&err);
        assert!(!hint.retryable);
        assert!(hint.detail.contains("磁盘空间"));
    }

    #[test]
    fn privacy_error_not_retryable() {
        let err = RamariaError::privacy("API key 缺失");
        let hint = ErrorHint::from_error(&err);
        assert!(!hint.retryable);
        assert!(hint.detail.contains("隐私确认"));
    }

    #[test]
    fn index_error_not_retryable() {
        let err = RamariaError::index("索引损坏");
        let hint = ErrorHint::from_error(&err);
        assert!(!hint.retryable);
        assert!(hint.detail.contains("重建索引"));
    }

    #[test]
    fn validation_error_not_retryable() {
        let err = RamariaError::validation("内容为空");
        let hint = ErrorHint::from_error(&err);
        assert!(!hint.retryable);
    }

    #[test]
    fn io_error_not_retryable() {
        let err = RamariaError::io("读取失败", None);
        let hint = ErrorHint::from_error(&err);
        assert!(!hint.retryable);
    }

    #[test]
    fn unsupported_error_not_retryable() {
        let err = RamariaError::unsupported("功能未实现");
        let hint = ErrorHint::from_error(&err);
        assert!(!hint.retryable);
        assert!(hint.detail.contains("升级"));
    }

    #[test]
    fn convenience_functions() {
        let err = RamariaError::llm("超时");
        assert_eq!(error_title(&err), "LLM 服务错误");
        assert!(is_retryable(&err));
        assert!(!error_detail(&err).is_empty());
    }
}
