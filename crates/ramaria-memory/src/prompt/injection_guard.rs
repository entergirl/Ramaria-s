//! rust/crates/ramaria-memory/src/prompt/injection_guard.rs - 线上记忆注入开关
//!
//! 设计特点:
//! - 根据 `BackendSelection.online_memory_injection` 控制是否向线上 LLM 注入记忆上下文
//! - 本地 provider（LM Studio）始终允许注入
//! - `MemoryInjectionStatus` 枚举清晰表达三种状态: 允许/已禁用/不适用
//! - 供 app 层在构建 ChatRequest 前调用，决定 memory_context 是否置空

use ramaria_core::types::LlmProvider;

// =========================================================
// 记忆注入状态
// =========================================================

/// 记忆上下文注入状态。
///
/// 变体:
/// - `Allowed`: 允许注入（本地 provider，或用户已授权线上注入）。
/// - `Disabled`: 用户禁用了线上记忆注入。
/// - `NotApplicable`: 无需考虑（无记忆上下文可注入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryInjectionStatus {
    /// 允许注入记忆上下文
    Allowed,
    /// 用户禁用了线上记忆注入
    Disabled,
    /// 无需考虑（无记忆上下文）
    NotApplicable,
}

impl MemoryInjectionStatus {
    /// 是否允许注入。
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

// =========================================================
// 注入检查
// =========================================================

/// 检查是否允许向 LLM 注入记忆上下文。
///
/// 规则:
/// 1. 若无记忆上下文 → `NotApplicable`
/// 2. provider 为本地（LmStudio）→ `Allowed`
/// 3. provider 为线上 + online_memory_injection = true → `Allowed`
/// 4. provider 为线上 + online_memory_injection = false → `Disabled`
///
/// 参数:
/// - `provider`: 当前 LLM provider。
/// - `online_injection_enabled`: BackendSelection.online_memory_injection 的值。
/// - `has_memory`: 是否有记忆上下文需要注入。
///
/// 返回:
/// - `MemoryInjectionStatus` 描述当前注入权限。
pub fn check_injection(
    provider: LlmProvider,
    online_injection_enabled: bool,
    has_memory: bool,
) -> MemoryInjectionStatus {
    if !has_memory {
        return MemoryInjectionStatus::NotApplicable;
    }

    // 本地 provider 始终允许
    if !provider.is_online() {
        return MemoryInjectionStatus::Allowed;
    }

    // 线上 provider 需要用户授权
    if online_injection_enabled {
        MemoryInjectionStatus::Allowed
    } else {
        tracing::debug!(
            provider = %provider,
            "线上记忆注入已被用户禁用"
        );
        MemoryInjectionStatus::Disabled
    }
}

/// 应用注入开关：若禁用则将 memory_context 置为 None。
///
/// 参数:
/// - `provider`: 当前 LLM provider。
/// - `online_injection_enabled`: BackendSelection.online_memory_injection。
/// - `memory_context`: 原始记忆上下文字符串。
///
/// 返回:
/// - 若允许注入 → `Some(memory_context)`。
/// - 若禁用 → `None`。
/// - 若无上下文 → `None`。
pub fn apply_injection_guard(
    provider: LlmProvider,
    online_injection_enabled: bool,
    memory_context: Option<String>,
) -> Option<String> {
    match memory_context {
        None => None,
        Some(ref ctx) if ctx.trim().is_empty() => None,
        Some(ctx) => {
            let status = check_injection(provider, online_injection_enabled, true);
            match status {
                MemoryInjectionStatus::Allowed => Some(ctx),
                MemoryInjectionStatus::Disabled => {
                    tracing::info!(
                        provider = %provider,
                        "记忆上下文已被线上注入开关过滤"
                    );
                    None
                }
                MemoryInjectionStatus::NotApplicable => None,
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

    #[test]
    fn local_provider_always_allowed() {
        let status = check_injection(LlmProvider::LmStudio, false, true);
        assert_eq!(status, MemoryInjectionStatus::Allowed);
    }

    #[test]
    fn online_provider_with_injection_on() {
        let status = check_injection(LlmProvider::DeepSeek, true, true);
        assert_eq!(status, MemoryInjectionStatus::Allowed);
    }

    #[test]
    fn online_provider_with_injection_off() {
        let status = check_injection(LlmProvider::DeepSeek, false, true);
        assert_eq!(status, MemoryInjectionStatus::Disabled);
    }

    #[test]
    fn no_memory_not_applicable() {
        let status = check_injection(LlmProvider::OpenAI, true, false);
        assert_eq!(status, MemoryInjectionStatus::NotApplicable);
    }

    #[test]
    fn apply_guard_online_disabled() {
        let result = apply_injection_guard(LlmProvider::DeepSeek, false, Some("敏感记忆".into()));
        assert!(result.is_none());
    }

    #[test]
    fn apply_guard_local_preserves() {
        let result = apply_injection_guard(LlmProvider::LmStudio, false, Some("记忆".into()));
        assert_eq!(result.as_deref(), Some("记忆"));
    }

    #[test]
    fn apply_guard_empty_string_treated_as_none() {
        let result = apply_injection_guard(LlmProvider::LmStudio, true, Some("   ".into()));
        assert!(result.is_none());
    }
}
