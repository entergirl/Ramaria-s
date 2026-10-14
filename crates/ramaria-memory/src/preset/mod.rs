//! crates/ramaria-memory/src/preset/mod.rs - Ramaria 预设格式解析模块
//!
//! 设计特点:
//! - 解析 persona.toml 超集：注册段 / 对话身份 / 自由文本块 / L3 结构化段 / 提示词补丁
//! - 统一中间表示 ParsedPreset 供导入写入管线与预览输出共用
//! - 分层宽容：文件级失败仅限 TOML 语法、主键校验与名称缺失，条目级问题降级为提示与跳过明细
//! - ST 角色卡（JSON / PNG tEXt chara）解析与字节级格式判定入口
//! - 中间表示到超集 TOML 的序列化（round-trip 可重放）与 dry-run 预览构建

mod model;
mod parse;
mod plan;
mod png;
mod preview;
mod ser;

#[cfg(test)]
mod tests;

pub use model::{
    ExampleEntry, FactEntry, HintLevel, IdentitySection, KindSource, ParsedPreset, PersonaSection,
    PresetHint, PromptPatch, RuleEntry, SkipCategory, SkipReason, SkippedEntry, TraitEntry,
    TraitsSection,
};
pub use parse::{parse_st_json, parse_st_png, parse_toml};
pub use plan::{
    DecomposedEntries, EntryDecision, ExistingProfile, ImportPlan, PlanOptions, Planned,
    PromptPlan, RegistrationPlan, ResolvedRegistration, plan_import, resolve_registration,
};
pub use png::extract_chara_text;
pub use preview::{
    BlocksPreview, CountPreview, DecomposePreview, IdentityPreview, PresetPreview, PreviewMode,
    PreviewOptions, PromptPreview, RegistrationPreview, SkipItem, TraitsPreview, WillWritePreview,
    build_preview, build_preview_from_plan,
};
pub use ser::to_superset_toml;

use ramaria_core::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};

// =========================================================
// 格式与解析上下文
// =========================================================

/// 预设文件格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetFormat {
    /// persona.toml 超集
    #[serde(rename = "toml")]
    Toml,
    /// SillyTavern 角色卡 JSON
    #[serde(rename = "st-json")]
    StJson,
    /// SillyTavern 角色卡 PNG（tEXt chara）
    #[serde(rename = "st-png")]
    StPng,
}

/// 解析上下文（文件与入口信息）。
#[derive(Debug, Clone)]
pub struct PresetParseContext {
    /// 文件名 stem（uid 与名称的兜底来源；None = 无文件上下文）
    pub file_stem: Option<String>,
    /// source 缺省值（预设导入 "manual"；目录通道 "file"）
    pub default_source: String,
}

impl Default for PresetParseContext {
    fn default() -> Self {
        Self {
            file_stem: None,
            default_source: "manual".to_string(),
        }
    }
}

// =========================================================
// 字节级统一入口
// =========================================================

/// 按内容判定格式并解析预设字节。
///
/// 判定顺序:
/// - PNG 签名 → ST 角色卡 PNG（tEXt chara）
/// - UTF-8 解码后 trim_start 以 `{` 开头 → ST 角色卡 JSON
/// - 其余 → persona.toml 超集
///
/// 参数:
/// - `bytes`: 预设文件原始字节。
/// - `ctx`: 解析上下文（文件名 stem 兜底与 source 缺省值）。
///
/// 返回:
/// - 文件级失败：字节非合法 UTF-8 / 对应格式的解析错误。
pub fn parse_preset(bytes: &[u8], ctx: &PresetParseContext) -> RamariaResult<ParsedPreset> {
    if png::is_png(bytes) {
        return parse::parse_st_png(bytes, ctx);
    }
    let content = std::str::from_utf8(bytes)
        .map_err(|e| RamariaError::config(format!("预设文件不是合法 UTF-8: {e}")))?;
    if content.trim_start().starts_with('{') {
        parse::parse_st_json(content, ctx)
    } else {
        parse::parse_toml(content, ctx)
    }
}
