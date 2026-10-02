//! crates/ramaria-core/src/types/style.rs - Ramaria 表达层风格数据类型模块
//!
//! 设计特点:
//! - 定义风格规则来源与统计状态枚举
//! - PersonaStyleStats 汇总表达层风格统计
//! - PersonaExample 承载用于少样本的表达示例
//! - 提供构造与查询辅助方法
//! - 所有类型支持 serde

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::now_ms;

// =========================================================
// 风格统计（persona_style_stats 表，表达层 A3）
// =========================================================

/// 风格规则文本来源（persona_style_stats.rule_source）。
///
/// 职责:
/// - 标记自动风格规则文本的生成方式：模板拼接（确定性）或 LLM 翻译增强。
/// - `None` = 未生成规则文本（数据不足 / 无显著项 / 风格关闭）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StyleRuleSource {
    /// 未生成规则文本
    #[default]
    None,
    /// 模板拼接生成（确定性可测，零 LLM 依赖）
    Template,
    /// LLM 离线翻译增强生成
    Llm,
}

impl StyleRuleSource {
    /// 返回数据库存储值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Template => "template",
            Self::Llm => "llm",
        }
    }
}

impl std::fmt::Display for StyleRuleSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 风格统计状态（persona_style_stats.status）。
///
/// 职责:
/// - 记录一次统计的结果状态，供注入侧判断是否读取 `rule_text`。
/// - `Insufficient`（数据不足）与 `NoSignificant`（无显著项）均不产生规则文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StyleStatsStatus {
    /// 样本量不足（n_p < 阈值）：标注数据不足，不生成规则
    #[default]
    Insufficient,
    /// 存在统计显著项，规则文本已生成（可注入）
    Ready,
    /// 样本量足够但无显著项：不生成规则，静默跳过
    NoSignificant,
}

impl StyleStatsStatus {
    /// 返回数据库存储值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Insufficient => "insufficient",
            Self::Ready => "ready",
            Self::NoSignificant => "no_significant",
        }
    }
}

impl std::fmt::Display for StyleStatsStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单行 persona 风格统计记录（persona_style_stats 表）。
///
/// 职责:
/// - 按 persona 单行 upsert（D-V17-003）：五维统计参数 + 样本量 + 基线引用 + 规则文本。
/// - `stats_json` 只含统计参数（频率/计数/分布），**不含原文消息文本**
///   （隐私红线：风格统计参数/基线池不含原文）。
///
/// 字段约定:
/// - `sample_count`: 统计样本量 n_p（persona 消息条数）。
/// - `baseline_version`: 全局基线池合并版本（更新时递增，供调试与基线引用）。
/// - `rule_text`: 生成的自动风格规则文本；`None` = 未生成（数据不足/无显著项/关闭）。
/// - `status`: 统计状态，`Ready` 才允许注入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaStyleStats {
    /// 人格标识（persona_style_stats.persona_uid，主键）
    pub persona_uid: String,
    /// 统计样本量 n_p（消息条数）
    pub sample_count: u32,
    /// 五维统计参数（结构化 JSON，不含原文）
    pub stats_json: String,
    /// 全局基线池合并版本（基线引用）
    pub baseline_version: u32,
    /// 自动风格规则文本（None = 未生成）
    pub rule_text: Option<String>,
    /// 规则文本来源（template / llm / none）
    pub rule_source: StyleRuleSource,
    /// 统计状态
    pub status: StyleStatsStatus,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

impl PersonaStyleStats {
    /// 创建一条新的风格统计记录（id 与时间由本方法生成）。
    pub fn new(
        persona_uid: String,
        sample_count: u32,
        stats_json: String,
        baseline_version: u32,
        rule_text: Option<String>,
        rule_source: StyleRuleSource,
        status: StyleStatsStatus,
    ) -> Self {
        Self {
            persona_uid,
            sample_count,
            stats_json,
            baseline_version,
            rule_text,
            rule_source,
            status,
            updated_at: now_ms(),
        }
    }
}

/// 对话 Few-shot 示例——注入 System Prompt 作为说话风格参考。
///
/// 职责:
/// - 从真实对话中选取 3-5 对示例，帮助 LLM 模仿该人格的说话风格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaExample {
    /// 内部索引（INTEGER AUTOINCREMENT）
    pub id: i64,
    pub persona_uid: String,
    /// 对方说了什么
    pub partner: String,
    /// 此人回复了什么
    pub reply: String,
    /// FK→sessions.id (TEXT/UUID)
    pub session_id: Option<Uuid>,
    /// 前文（最多前 3 条）
    pub context: Option<String>,
    pub valence: f64,
    /// 话题标签，逗号分隔
    pub tags: Option<String>,
    /// 1=当前生效，0=候选库备选
    pub selected: bool,
    /// 回复字符数
    pub length: i32,
    pub created_at: i64,
}

impl PersonaExample {
    /// 创建新的对话示例。id 为 0，由存储层回填。
    pub fn new(persona_uid: String, partner: String, reply: String) -> Self {
        let len = reply.chars().count() as i32;
        Self {
            id: 0,
            persona_uid,
            partner,
            reply,
            session_id: None,
            context: None,
            valence: 0.0,
            tags: None,
            selected: false,
            length: len,
            created_at: now_ms(),
        }
    }
}
