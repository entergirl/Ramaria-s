//! crates/ramaria-service/src/recall/policy.rs - 召回隐私与边界策略
//!
//! 设计特点:
//! - 承载「谁可见 / 原文是否出端」两类由配置决定的边界，在服务层强制执行
//!   （协议壳只做参数校验，不承担隐私判断）
//! - 装配缺省按配置闸门映射：`allow_raw_text = [injection].utt × [utt].enabled`
//! - 显式注入的保守值独立于缺省：原文是最高敏感层，默认不返回
//! - 人格白名单 `["*"]` 表示全部可见；空列表兜底为全可见，避免"空即全禁"的误配
//! - 纯数据与判定：无 I/O，宿主经 `Engine::set_recall_policy` 整体替换

use ramaria_core::config::RamariaConfig;

// =========================================================
// 召回策略（入口层注入）
// =========================================================

/// 召回隐私与边界策略。
///
/// 职责:
/// - 承载「谁可见 / 原文是否出端 / 默认预算」这三类由配置决定的边界，
///   在服务层强制执行（协议壳只做参数校验，不承担隐私判断）。
///
/// 字段约定:
/// - `allow_raw_text`: 是否允许返回 utt 原文块（装配缺省按配置闸门映射，见
///   [`RecallPolicy::from_config`]；显式注入的保守值为 false —— 原文是最高敏感层）；
/// - `allowed_personas`: 可见人格白名单，`["*"]` 表示全部可见。
#[derive(Debug, Clone, PartialEq)]
pub struct RecallPolicy {
    pub allow_raw_text: bool,
    pub allowed_personas: Vec<String>,
}

impl Default for RecallPolicy {
    /// 默认最保守：原文不出端、全部人格可见（由用户后续收紧）。
    fn default() -> Self {
        Self {
            allow_raw_text: false,
            allowed_personas: vec!["*".to_string()],
        }
    }
}

impl RecallPolicy {
    /// 从配置映射缺省策略（装配层缺省口径）。
    ///
    /// 职责:
    /// - 把配置闸门映射为服务层缺省策略：`allow_raw_text = [injection].utt × [utt].enabled`；
    /// - 人格白名单缺省不收紧（`["*"]` 全部可见），由宿主按需注入。
    ///
    /// 参数:
    /// - `config`: 当前生效配置。
    ///
    /// 返回:
    /// - 原文开关与配置闸门一致的策略快照。
    ///
    /// 说明:
    /// - 宿主可在装配后经 `Engine::set_recall_policy` 覆盖收紧（如关闭原文 / 限制人格）；
    ///   显式注入方不受缺省影响（注入值整体替换）。
    pub fn from_config(config: &RamariaConfig) -> Self {
        Self {
            allow_raw_text: config.injection.utt && config.utt.enabled,
            allowed_personas: vec!["*".to_string()],
        }
    }

    /// 链式设置原文开关。
    pub fn with_allow_raw_text(mut self, allow: bool) -> Self {
        self.allow_raw_text = allow;
        self
    }

    /// 链式设置人格白名单（空列表按 `["*"]` 处理，避免"空即全禁"的误配）。
    pub fn with_allowed_personas(mut self, personas: Vec<String>) -> Self {
        self.allowed_personas = if personas.is_empty() {
            vec!["*".to_string()]
        } else {
            personas
        };
        self
    }

    /// 判定某人格是否可见。
    ///
    /// 参数:
    /// - `persona_uid`: 待判定人格。
    ///
    /// 返回:
    /// - `true`: 白名单含 `*` 或该人格。
    pub fn persona_allowed(&self, persona_uid: &str) -> bool {
        self.allowed_personas
            .iter()
            .any(|rule| rule == "*" || rule == persona_uid)
    }
}
