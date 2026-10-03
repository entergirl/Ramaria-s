//! crates/ramaria-service/src/error_hint.rs - 错误到 UI/CLI 提示映射
//!
//! 设计特点:
//! - 将 `RamariaError::category` 映射为面向最终用户的友好提示
//! - 每条提示包含: 简短摘要（title）+ 详细建议（detail）
//! - 支持可重试标记（retryable），供 UI 决定是否显示"重试"按钮
//! - 未识别类别保守提示"查看日志"，不泄露内部错误细节
//! - 提供入口统一文案映射（`entry_error_message`）：业务类原文直出，技术类单一场景前缀
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
    /// - `err`: 调用方捕获的统一错误。
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
    /// - `embedding`: 嵌入模型错误，需检查模型文件/磁盘空间 → 不可重试
    /// - `serialization`: 数据序列化错误，需重启应用 → 不可重试
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

            "embedding" => Self {
                title: "嵌入模型错误".to_string(),
                detail: concat!(
                    "本地嵌入模型加载或推理失败。\n",
                    "建议：检查模型文件是否完整、磁盘空间是否充足；可在设置中重新下载模型。\n",
                    "如果问题持续，请重启应用。"
                )
                .to_string(),
                retryable: false,
            },

            "serialization" => Self {
                title: "数据序列化错误".to_string(),
                detail: concat!(
                    "处理数据格式时出错。\n",
                    "建议：重启应用后重试。\n",
                    "如果问题持续，请导出诊断包并反馈。"
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
// 入口统一文案映射
// =========================================================

/// 入口统一错误文案（单一映射点，各入口只调用一次）。
///
/// 参数:
/// - `err`: 服务层返回的统一错误。
/// - `scene`: 调用场景描述（如 `"查询记忆失败"`）；为空时不加场景前缀。
///
/// 返回:
/// - `validation` / `privacy`: 错误上下文原文直出（业务文案逐字保留，不叠加任何前缀）；
/// - 其余类别: `{scene}: {类别标题}: {原因}`（类别标题取 [`ErrorHint`] 中文标题，原因取 `err.context()`）。
///
/// 说明:
/// - 不重复拼接错误类别串（不使用 `RamariaError` 的 Display），避免双重前缀；
/// - 文案仅含类别标题与已面向用户的上下文，不携带 source 链等诊断细节。
pub fn entry_error_message(err: &RamariaError, scene: &str) -> String {
    match err.category() {
        "validation" | "privacy" => err.context().to_string(),
        _ => {
            let title = ErrorHint::from_error(err).title;
            if scene.is_empty() {
                format!("{title}: {}", err.context())
            } else {
                format!("{scene}: {title}: {}", err.context())
            }
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// ErrorHint::from_error 各错误类型变体参数化验证。
    #[test]
    fn error_hint_variants() {
        // (err, 期望 title（空=不断言）, 期望 retryable, 期望 detail 子串（None=不断言）)
        let cases: Vec<(RamariaError, &str, bool, Option<&str>)> = vec![
            (
                RamariaError::config("配置缺失"),
                "配置错误",
                false,
                Some("设置向导"),
            ),
            (RamariaError::llm("连接超时"), "LLM 服务错误", true, None),
            (
                RamariaError::storage("数据库损坏"),
                "",
                false,
                Some("磁盘空间"),
            ),
            (
                RamariaError::privacy("API key 缺失"),
                "",
                false,
                Some("隐私确认"),
            ),
            (RamariaError::index("索引损坏"), "", false, Some("重建索引")),
            (RamariaError::validation("内容为空"), "", false, None),
            (RamariaError::io("读取失败", None), "", false, None),
            (
                RamariaError::embedding("模型加载失败"),
                "嵌入模型错误",
                false,
                Some("重新下载模型"),
            ),
            (
                RamariaError::serialization("JSON 解析失败"),
                "数据序列化错误",
                false,
                Some("重启应用"),
            ),
            (
                RamariaError::unsupported("功能未实现"),
                "",
                false,
                Some("升级"),
            ),
        ];
        for (err, title, retryable, detail_substr) in cases {
            let hint = ErrorHint::from_error(&err);
            if !title.is_empty() {
                assert_eq!(hint.title, title, "{err:?}");
            }
            assert_eq!(hint.retryable, retryable, "{err:?}");
            if let Some(substr) = detail_substr {
                assert!(hint.detail.contains(substr), "{err:?}");
            }
        }
    }

    #[test]
    fn convenience_functions() {
        let err = RamariaError::llm("超时");
        assert_eq!(error_title(&err), "LLM 服务错误");
        assert!(is_retryable(&err));
        assert!(!error_detail(&err).is_empty());
    }

    /// 入口统一文案：业务类（validation / privacy）原文直出，不叠加场景与类别前缀。
    #[test]
    fn entry_error_message_business_categories_are_verbatim() {
        let validation = RamariaError::validation("会话不存在: abc");
        assert_eq!(
            entry_error_message(&validation, "查询会话失败"),
            "会话不存在: abc"
        );

        let privacy = RamariaError::privacy("请先完成隐私确认");
        assert_eq!(
            entry_error_message(&privacy, "生成回复失败"),
            "请先完成隐私确认"
        );
    }

    /// 入口统一文案：技术类 = `{场景}: {类别标题}: {原因}`（标题取 error_hint，不复现英文类别串）。
    #[test]
    fn entry_error_message_technical_categories_have_scene_and_title() {
        let cases: Vec<(RamariaError, &str)> = vec![
            (RamariaError::storage("磁盘只读"), "数据库错误"),
            (RamariaError::llm("连接超时"), "LLM 服务错误"),
            (RamariaError::config("缺少必需字段"), "配置错误"),
            (RamariaError::index("索引损坏"), "索引错误"),
            (RamariaError::io("读取失败", None), "文件读写错误"),
            (RamariaError::embedding("模型加载失败"), "嵌入模型错误"),
            (
                RamariaError::serialization("JSON 解析失败"),
                "数据序列化错误",
            ),
            (RamariaError::unsupported("功能未实现"), "功能不可用"),
        ];
        for (err, title) in cases {
            let message = entry_error_message(&err, "查询记忆失败");
            assert_eq!(
                message,
                format!("查询记忆失败: {title}: {}", err.context()),
                "{err:?}"
            );
            assert!(!message.contains("error:"), "不应复现英文类别串: {message}");
        }
    }

    /// 入口统一文案：场景为空时退化为 `{类别标题}: {原因}`（不伪造场景词）。
    #[test]
    fn entry_error_message_without_scene_omits_prefix() {
        let err = RamariaError::storage("磁盘只读");
        assert_eq!(entry_error_message(&err, ""), "数据库错误: 磁盘只读");
    }
}
