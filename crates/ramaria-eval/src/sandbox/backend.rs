//! crates/ramaria-eval/src/sandbox/backend.rs - Ramaria 评测沙盒后端解析与应用
//!
//! 设计特点:
//! - 三种来源统一解析：继承用户配置 / 独立本地模型 / 独立云端模型，运行体只消费解析结果
//! - 可见文本只区分本地与云端模型：报告、摘要与告警不出现具体厂商名
//! - 降级不阻塞：继承来源缺失 / 不可读 / 不可用时回退产品默认，并留下一条可读告警
//! - 应用结果可解释：配置写入、向量模型与连通性各自给出状态，失败只降级对应通道
//! - 用户库只读：继承来源经服务层只读用例读取，不迁移、不写入用户数据

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{BackendConfig, LlmProvider as LlmProviderKind};
use ramaria_service::{Engine, read_backend_config_at};

#[cfg(test)]
mod tests;

// =========================================================
// 常量
// =========================================================

/// 本地模型未填地址时使用的默认地址（界面预填与解析共用）。
pub(crate) const LOCAL_DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";

/// 来源描述：产品默认。
const SOURCE_PRODUCT_DEFAULT: &str = "产品默认";

/// 来源描述：用户库内保存的配置。
const SOURCE_USER_CONFIG: &str = "用户配置";

/// 来源描述：本次运行的独立配置。
const SOURCE_INDEPENDENT: &str = "独立配置";

/// 来源描述：产品默认且本次应用失败。
const SOURCE_PRODUCT_DEFAULT_APPLY_FAILED: &str = "产品默认（应用失败）";

/// 模型标识为空时的显示文本。
const AUTO_MODEL_TEXT: &str = "自动选择";

// =========================================================
// 后端类别与来源
// =========================================================

/// 沙盒后端类别。
///
/// 说明:
/// - 只用于评测侧可见文本与报告，内部仍映射到具体 provider 构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxModelKind {
    /// 本地模型（本机可直连的模型服务）
    Local,
    /// 云端模型（需网络与密钥）
    Cloud,
}

impl SandboxModelKind {
    /// 返回稳定小写标识（行模式与产物字段使用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }

    /// 返回面向用户的标签。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "本地模型",
            Self::Cloud => "云端模型",
        }
    }
}

/// 后端来源模式。
///
/// 字段约定:
/// - `Inherit`: 继承用户库中的后端配置（缺失 / 不可用时回退产品默认）；
/// - `Local` / `Cloud`: 本次运行使用独立配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxBackendMode {
    /// 继承当前用户配置
    #[default]
    Inherit,
    /// 独立本地模型
    Local,
    /// 独立云端模型
    Cloud,
}

impl SandboxBackendMode {
    /// 返回稳定小写标识（行模式指令值与产物字段使用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }

    /// 返回面向用户的标签。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Inherit => "继承当前配置",
            Self::Local => "本地模型",
            Self::Cloud => "云端模型",
        }
    }
}

/// 后端来源选择。
///
/// 字段约定:
/// - `Independent.base_url`: 留空时本地模型回退默认地址，云端模型按校验错误拒绝；
/// - `Independent.model_id`: 本地模型允许留空（由服务端自动选择），云端模型必填；
/// - `Independent.embedding_model_path`: 可选向量模型目录，写入后端配置的持久化路径。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SandboxBackendChoice {
    /// 继承用户配置
    #[default]
    Inherit,
    /// 本次运行的独立配置
    Independent {
        /// 后端类别
        kind: SandboxModelKind,
        /// 模型服务地址
        base_url: String,
        /// 模型标识
        model_id: String,
        /// 向量模型目录
        embedding_model_path: Option<PathBuf>,
    },
}

// =========================================================
// 解析结果与报告
// =========================================================

/// 解析后的后端（应用前）。
///
/// 字段约定:
/// - `source`: 来源描述（用户配置 / 独立配置 / 产品默认）；
/// - `config`: 待应用的后端配置；
/// - `fallback`: 有值表示已回退，调用方须写入告警。
#[derive(Debug, Clone)]
pub struct ResolvedBackend {
    /// 来源模式
    pub mode: SandboxBackendMode,
    /// 来源描述
    pub source: String,
    /// 后端类别
    pub kind: SandboxModelKind,
    /// 待应用的后端配置
    pub config: BackendConfig,
    /// 降级原因（None = 未发生降级）
    pub fallback: Option<String>,
}

/// 后端应用结果报告。
///
/// 字段约定:
/// - `source`: 实际生效的来源描述；
/// - `model`: 实际生效的模型标识（空 = 服务端自动选择）；
/// - `llm_status` / `embedding_status`: 连通性与向量通道状态文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendReport {
    /// 来源模式
    pub mode: SandboxBackendMode,
    /// 来源描述
    pub source: String,
    /// 后端类别
    pub kind: SandboxModelKind,
    /// 模型标识
    pub model: String,
    /// 模型服务地址
    pub base_url: String,
    /// 连通性状态
    pub llm_status: String,
    /// 向量通道状态
    pub embedding_status: String,
}

impl BackendReport {
    /// 模型标识的显示文本（空值显示为自动选择）。
    pub fn model_text(&self) -> &str {
        let trimmed = self.model.trim();
        if trimmed.is_empty() {
            AUTO_MODEL_TEXT
        } else {
            trimmed
        }
    }
}

// =========================================================
// 解析
// =========================================================

/// 解析后端来源为可应用的后端配置。
///
/// 参数:
/// - `choice`: 本次运行的后端来源选择；
/// - `user_db`: 用户库路径（仅继承模式使用；`None` 表示无宿主库）。
///
/// 返回:
/// - 成功时返回可应用的配置；继承模式下的缺失 / 不可用一律回退产品默认，
///   并把原因写入 `fallback`（不报错）；
/// - 独立云端配置缺地址或模型标识返回 `Validation`。
pub async fn resolve_backend(
    choice: &SandboxBackendChoice,
    user_db: Option<&Path>,
) -> RamariaResult<ResolvedBackend> {
    match choice {
        SandboxBackendChoice::Inherit => Ok(resolve_inherit(user_db).await),
        SandboxBackendChoice::Independent {
            kind,
            base_url,
            model_id,
            embedding_model_path,
        } => resolve_independent(*kind, base_url, model_id, embedding_model_path.as_deref()),
    }
}

/// 继承用户配置；任何不可用情形回退产品默认并留下降级原因。
async fn resolve_inherit(user_db: Option<&Path>) -> ResolvedBackend {
    let Some(db_path) = user_db else {
        return product_default("未提供用户库路径，使用本地默认模型");
    };

    match read_backend_config_at(db_path).await {
        Ok(None) => product_default("未找到用户后端配置，使用本地默认模型"),
        Ok(Some(config)) => {
            if config.base_url.trim().is_empty() {
                return product_default("用户后端配置缺少地址，使用本地默认模型");
            }
            if config.provider.is_online() && config.capability.model_id.trim().is_empty() {
                return product_default("用户后端配置缺少模型标识，使用本地默认模型");
            }
            let kind = if config.provider.is_online() {
                SandboxModelKind::Cloud
            } else {
                SandboxModelKind::Local
            };
            ResolvedBackend {
                mode: SandboxBackendMode::Inherit,
                source: SOURCE_USER_CONFIG.to_string(),
                kind,
                config,
                fallback: None,
            }
        }
        Err(error) => product_default(format!("读取用户后端配置失败，使用本地默认模型: {error}")),
    }
}

/// 构造"产品默认后端 + 降级原因"的解析结果。
fn product_default(reason: impl Into<String>) -> ResolvedBackend {
    ResolvedBackend {
        mode: SandboxBackendMode::Inherit,
        source: SOURCE_PRODUCT_DEFAULT.to_string(),
        kind: SandboxModelKind::Local,
        config: BackendConfig::lm_studio_default(),
        fallback: Some(reason.into()),
    }
}

/// 按地址识别云端 provider。
///
/// 说明:
/// - DeepSeek 官方域名的凭据条目与通用 OpenAI 兼容服务不同，必须选用
///   `DeepSeek` provider 才能读到同一份系统凭据；
/// - 其余云端地址按通用 OpenAI 兼容服务处理。
fn cloud_provider(base_url: &str) -> LlmProviderKind {
    if base_url.contains("api.deepseek.com") {
        LlmProviderKind::DeepSeek
    } else {
        LlmProviderKind::OpenAI
    }
}

/// 解析独立配置：本地模型补默认地址，云端模型缺项直接拒绝。
fn resolve_independent(
    kind: SandboxModelKind,
    base_url: &str,
    model_id: &str,
    embedding_model_path: Option<&Path>,
) -> RamariaResult<ResolvedBackend> {
    let base_url = base_url.trim();
    let model_id = model_id.trim();

    let (provider, base_url, mode, fallback) = match kind {
        SandboxModelKind::Local => {
            if base_url.is_empty() {
                (
                    LlmProviderKind::LmStudio,
                    LOCAL_DEFAULT_BASE_URL,
                    SandboxBackendMode::Local,
                    Some("未填写地址，使用本地默认地址".to_string()),
                )
            } else {
                (
                    LlmProviderKind::LmStudio,
                    base_url,
                    SandboxBackendMode::Local,
                    None,
                )
            }
        }
        SandboxModelKind::Cloud => {
            if base_url.is_empty() {
                return Err(RamariaError::validation("云端模型必须填写地址"));
            }
            if model_id.is_empty() {
                return Err(RamariaError::validation("云端模型必须填写模型标识"));
            }
            (
                cloud_provider(base_url),
                base_url,
                SandboxBackendMode::Cloud,
                None,
            )
        }
    };

    let mut config =
        BackendConfig::new_with_defaults(provider, base_url.to_string(), model_id.to_string());
    config.embedding_model_path =
        embedding_model_path.map(|path| path.to_string_lossy().into_owned());

    Ok(ResolvedBackend {
        mode,
        source: SOURCE_INDEPENDENT.to_string(),
        kind,
        config,
        fallback,
    })
}

// =========================================================
// 应用
// =========================================================

/// 把解析结果应用到已装配引擎，并汇总后端报告与告警。
///
/// 参数:
/// - `engine`: 沙盒引擎（装配阶段使用产品默认后端）；
/// - `resolved`: 解析结果（含降级原因）。
///
/// 返回:
/// - `BackendReport`: 实际生效的来源、模型、地址与连通性；
/// - `Vec<String>`: 降级原因（顺序为：来源降级 → 配置应用 → 向量模型 → 连通性）。
///
/// 说明:
/// - 配置应用失败时继续用产品默认后端跑完本次评测，只在报告里如实标注；
/// - 向量模型与连通性失败只降级对应通道，不中止运行。
pub async fn apply_backend(
    engine: &Engine,
    resolved: ResolvedBackend,
) -> (BackendReport, Vec<String>) {
    let ResolvedBackend {
        mode,
        source,
        mut kind,
        config,
        fallback,
    } = resolved;

    let mut warnings: Vec<String> = fallback.into_iter().collect();

    let outcome = engine.update_backend_config(&config, None).await;
    let (source, config) = match outcome {
        Ok(()) => (source, config),
        Err(error) => {
            warnings.push(format!("后端配置应用失败，继续使用默认模型: {error}"));
            tracing::warn!(error = %error, "沙盒后端配置应用失败");
            // 回退到默认本地配置后，报告类别必须与实际生效的配置一致。
            kind = SandboxModelKind::Local;
            (
                SOURCE_PRODUCT_DEFAULT_APPLY_FAILED.to_string(),
                BackendConfig::lm_studio_default(),
            )
        }
    };

    let embedding_status = apply_embedding(engine, &config, &mut warnings).await;
    let llm_status = probe_llm(engine, &mut warnings).await;

    let report = BackendReport {
        mode,
        source,
        kind,
        model: config.capability.model_id.clone(),
        base_url: config.base_url.clone(),
        llm_status,
        embedding_status,
    };
    (report, warnings)
}

/// 应用向量模型：路径为空按未配置处理，加载失败只降级向量通道。
async fn apply_embedding(
    engine: &Engine,
    config: &BackendConfig,
    warnings: &mut Vec<String>,
) -> String {
    let path = config
        .embedding_model_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let Some(path) = path else {
        warnings.push("未配置向量模型，向量通道降级".to_string());
        return "未配置".to_string();
    };

    match engine.save_embedding_model(Some(path)).await {
        Ok(()) => match engine.embedding_model().await {
            Ok(Some(view)) => match view.dimension {
                Some(dimension) => format!("已加载（{dimension} 维）"),
                None => "已加载".to_string(),
            },
            _ => "已加载".to_string(),
        },
        Err(error) => {
            warnings.push(format!("向量模型加载失败，向量通道降级: {error}"));
            tracing::warn!(error = %error, "沙盒向量模型加载失败");
            "加载失败".to_string()
        }
    }
}

/// 探测模型服务连通性；失败只记录告警与日志，不把详细错误写进报告。
async fn probe_llm(engine: &Engine, warnings: &mut Vec<String>) -> String {
    match engine.llm().health_check().await {
        Ok(()) => "可达".to_string(),
        Err(error) => {
            warnings.push("模型服务不可达，封存与回答将降级".to_string());
            tracing::warn!(error = %error, "沙盒模型服务探测失败");
            "不可达".to_string()
        }
    }
}

// =========================================================
// 文本摘要
// =========================================================

/// 单行后端摘要（来源 / 模型 / 地址 / 连通性 / 向量）。
///
/// 说明:
/// - 只使用本地 / 云端两类词汇，不出现具体厂商名。
pub fn summary(report: &BackendReport) -> String {
    format!(
        "后端: {}（{}）/ 模型: {} / 地址: {} / 连接: {} / 向量: {}",
        report.source,
        report.kind.label(),
        report.model_text(),
        report.base_url,
        report.llm_status,
        report.embedding_status
    )
}
