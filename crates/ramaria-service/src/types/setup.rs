//! crates/ramaria-service/src/types/setup.rs - Ramaria 首次配置与模型管理用例数据结构
//!
//! 设计特点:
//! - 状态检查结果同时服务于缺项诊断与设置页展示（is_complete / missing_items）
//! - 设置请求承载后端选择；API key 仅经 OS keychain 落盘，不写入配置表
//! - 嵌入模型校验失败不返回错误，以 valid=false + reason 表达
//! - 降级原因供设置页翻译为可操作提示，非降级状态下不产生枚举值

use ramaria_core::types::LlmProvider;
use serde::{Deserialize, Serialize};

// =========================================================
// 首次配置用例（状态机推进与缺项诊断）
// =========================================================

/// 设置检查结果——列出当前还缺哪些配置。
///
/// 职责:
/// - 供设置页 / 配置向导展示"还差什么才能对话"（缺项清单）；
/// - `is_complete` 表示核心配置就绪（可对话）；嵌入模型缺失不影响该结论
///   （向量通道降级，BM25 + 关键词镜像仍可用）。
///
/// 字段约定:
/// - `backend_configured`: `backend_config` 是否已有记录。
/// - `model_selected`: 线上 provider 已填 model_id；本地 provider 视为已选
///   （模型由本地推理服务侧决定，配置层不强制）。
/// - `needs_indexing`: 记忆索引尚未构建（`schema_meta.index_version == 0`；
///   该键缺失时按未构建口径返回 `0`）。
/// - `embedding_available`: 嵌入模型已加载且可用（向量通道就绪）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStatus {
    pub backend_configured: bool,
    pub model_selected: bool,
    pub needs_indexing: bool,
    pub embedding_available: bool,
}

impl SetupStatus {
    /// 核心配置是否就绪（嵌入模型缺失不影响此结果）。
    pub fn is_complete(&self) -> bool {
        self.backend_configured && self.model_selected && !self.needs_indexing
    }

    /// 缺失项的人类可读描述列表（顺序与配置向导步骤一致）。
    pub fn missing_items(&self) -> Vec<&'static str> {
        let mut items = Vec::new();
        if !self.backend_configured {
            items.push("后端配置未完成（需选择 LLM provider）");
        }
        if !self.model_selected {
            items.push("模型未选择（需指定使用的模型）");
        }
        if self.needs_indexing {
            items.push("记忆索引待构建");
        }
        if !self.embedding_available {
            items.push("嵌入模型未配置（向量检索不可用，BM25+图谱仍可用）");
        }
        items
    }
}

/// 首次配置请求（配置向导提交的后端选择）。
///
/// 字段约定:
/// - `provider`: LLM 服务类型（本地 / 线上）。
/// - `model_id`: 模型标识（本地 provider 允许为空，由本地服务侧决定）。
/// - `base_url`: API 基础地址。
/// - `api_key`: 线上 provider 的 API key；本地 provider 忽略。
///   仅经 OS keychain 落盘，不写入配置表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRequest {
    pub provider: LlmProvider,
    pub model_id: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

// =========================================================
// 模型管理用例（嵌入模型校验 / 读取 / 降级原因）
// =========================================================

/// 嵌入模型校验结果。
///
/// 职责:
/// - 承载"用户指定目录能否作为嵌入模型使用"的判定结果；
/// - 校验不通过（目录缺失 / 加载失败 / 推理失败）时不返回错误，
///   而是以 `valid=false` + `reason` 表达，便于设置页直接展示原因。
///
/// 字段约定:
/// - `valid`: 目录存在、模型可加载且推理可执行时为 true。
/// - `dimension`: 模型可加载时的向量维度；加载失败（无法读出维度）时为 None。
/// - `reason`: 未通过时的可读原因（模型文件缺失 / 推理失败的具体信息）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingValidation {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl EmbeddingValidation {
    /// 构造失败结果（无维度，附原因）。
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            valid: false,
            dimension: None,
            reason: Some(reason.into()),
        }
    }
}

/// 嵌入模型配置视图（设置页读取当前生效的嵌入模型）。
///
/// 字段约定:
/// - `model_path`: 仅在"未加载但配置中留有路径"时填充（供 UI 预填输入框）；
///   模型已加载时不暴露本地路径（只暴露维度与可用性）。
/// - `valid`: 模型已加载且自测可用；未加载时为 false。
/// - `dimension`: 模型已加载时的向量维度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModelView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
}

/// 应用处于降级状态时的原因分类。
///
/// 职责:
/// - 供设置页把"降级"翻译成可操作提示（缺嵌入模型 / LLM 不可达）；
/// - 非降级状态下用例返回 None，不产生本枚举值。
///
/// 格式:
/// - 序列化为小写蛇形字符串：`embedding_missing` / `llm_unavailable` /
///   `both_unavailable` / `unknown`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DegradedReason {
    /// 嵌入模型缺失或不可用（向量通道不可用，BM25 + 关键词镜像仍可用）。
    EmbeddingMissing,
    /// LLM provider 不可达。
    LlmUnavailable,
    /// LLM 与嵌入模型同时不可用。
    BothUnavailable,
    /// 其它未知原因（两者均可用但仍处于降级状态）。
    Unknown,
}
