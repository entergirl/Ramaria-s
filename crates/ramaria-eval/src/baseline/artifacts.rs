//! crates/ramaria-eval/src/baseline/artifacts.rs - Ramaria 评测语料证据映射
//!
//! 设计特点:
//! - 语料 artifact 到库内消息的映射单独成档，作为档位产物的可选附件，不进清单也不改清单版本
//! - 顺序完全确定：样本与 artifact 都用 BTreeMap，写出为 pretty JSON 并以换行收尾
//! - 采集只读档位库：按外部会话标识定位会话，条数与正文逐条对齐，不符即中止
//! - 读取做四项校验（类型 / 版本 / 目录名 / 语料摘要），不一致报校验错误并带路径
//! - 写盘先落临时文件再改名，中断不会留下半份映射

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{Engine, SessionBrowseRequest, SessionMessageView, SessionMessagesRequest};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::corpus::{CorpusFileSpec, CorpusSpec, Sample, Session};

use super::build::CONVERSATION_PREFIX;
use super::tier::TierSpec;

#[cfg(test)]
mod tests;

/// 映射文件名（与档位库 / 清单 / qrels 同级）。
pub const ARTIFACTS_FILE: &str = "artifacts.json";

/// 标记产物类型的固定字符串。
pub const ARTIFACT_KIND: &str = "ramaria_eval_artifacts";

/// 映射文件自身的 schema 版本（独立于基线清单版本）。
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;

// =========================================================
// 数据结构
// =========================================================

/// 语料来源口径（与清单同值，便于脱离清单单独校验）。
///
/// 字段约定:
/// - `name` / `version`: 语料登记名与版本；
/// - `sha256`: 该语言文件的正文摘要，与清单 `corpus.sha256` 逐字相等。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusFacts {
    pub name: String,
    pub version: String,
    pub sha256: String,
}

impl CorpusFacts {
    /// 由语料登记项构造（名称与版本取语料，摘要取本次语言文件）。
    pub fn from_spec(spec: &CorpusSpec, file: &CorpusFileSpec) -> Self {
        Self {
            name: spec.name.to_string(),
            version: spec.version.to_string(),
            sha256: file.sha256.to_string(),
        }
    }
}

/// 一个档位的 artifact ↔ 消息映射（`artifacts.json`）。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物识别与版本；
/// - `tier_id`: 档位标识，必须与所在目录名一致；
/// - `corpus`: 语料来源口径；
/// - `samples`: 样本键（语料 user_key）→ artifact 标识 → 消息 id 列表；
///   列表保持语料消息顺序，同一个 artifact 的多条消息按出现顺序追加。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMap {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub tier_id: String,
    pub corpus: CorpusFacts,
    pub samples: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl ArtifactMap {
    /// 空映射（采集入口用，样本逐个填入）。
    fn new(tier_id: &str, corpus: CorpusFacts) -> Self {
        Self {
            artifact_kind: ARTIFACT_KIND.to_string(),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            tier_id: tier_id.to_string(),
            corpus,
            samples: BTreeMap::new(),
        }
    }

    /// 映射内的 artifact 总数（报告口径）。
    pub fn artifact_total(&self) -> usize {
        self.samples
            .values()
            .map(|artifacts| artifacts.len())
            .sum::<usize>()
    }
}

// =========================================================
// 会话对齐
// =========================================================

/// 把一天的语料会话与库内消息逐条对齐。
///
/// 参数:
/// - `session`: 语料会话（消息按文件顺序）；
/// - `messages`: 库内该会话的消息（服务层按时间正序返回）。
///
/// 返回:
/// - 按语料顺序排列的 `(artifact 标识, 消息 id)` 对；
/// - 条数或正文不一致返回 `Validation`，文案带日期与序号。
///
/// 说明: 只认条数与正文，不比角色与时间戳——角色由语料 speaker 决定，时间戳由灌入时补齐；
/// 正文比较去掉首尾空白（灌入侧按裁剪后的正文落库，语料里存在带尾随空格的条目）。
pub fn pair_session(
    session: &Session,
    messages: &[SessionMessageView],
) -> RamariaResult<Vec<(String, String)>> {
    let date = session.date.format("%Y-%m-%d");
    if session.messages.len() != messages.len() {
        return Err(RamariaError::validation(format!(
            "{date}: 会话消息条数不符（语料 {} 条 / 库内 {} 条）",
            session.messages.len(),
            messages.len()
        )));
    }

    let mut pairs = Vec::with_capacity(messages.len());
    for (index, (source, stored)) in session.messages.iter().zip(messages).enumerate() {
        if source.text.trim() != stored.content.trim() {
            return Err(RamariaError::validation(format!(
                "{date}: 第 {} 条消息正文不符（语料与库内不一致）",
                index + 1
            )));
        }
        pairs.push((source.artifact_id(), stored.id.to_string()));
    }
    Ok(pairs)
}

// =========================================================
// 采集
// =========================================================

/// 从档位库采集 artifact 映射。
///
/// 参数:
/// - `engine`: 档位库引擎（只读使用）；
/// - `samples`: 参与该档位的语料样本（按语料顺序）；
/// - `window`: 档位窗口，决定收录哪些日期的会话；
/// - `tier_id`: 档位标识，写入映射并用于读取校验；
/// - `corpus`: 语料来源口径。
///
/// 返回:
/// - 每个样本一项映射；窗口内没有会话的样本记为空表；
/// - 会话缺失、条数不符或正文不符返回 `Validation`，文案带日期与序号。
pub async fn collect(
    engine: &Engine,
    samples: &[Sample],
    window: &TierSpec,
    tier_id: &str,
    corpus: CorpusFacts,
) -> RamariaResult<ArtifactMap> {
    let sessions = session_index(engine).await?;
    let mut map = ArtifactMap::new(tier_id, corpus);

    for sample in samples {
        let mut artifacts: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut in_window: Vec<&Session> = sample
            .sessions
            .iter()
            .filter(|session| window.includes_date(session.date))
            .collect();
        in_window.sort_by_key(|session| session.date);

        for session in in_window {
            let date = session.date.format("%Y-%m-%d").to_string();
            let reference = format!("{CONVERSATION_PREFIX}:{}:{date}", sample.user_key);
            let session_id = sessions.get(&reference).copied().ok_or_else(|| {
                RamariaError::validation(format!(
                    "{date}: 档位 {tier_id} 的库内缺少会话 {reference}"
                ))
            })?;
            let page = engine
                .session_messages(SessionMessagesRequest {
                    session_id,
                    limit: None,
                    offset: None,
                })
                .await?;
            if page.total as usize != session.messages.len() {
                return Err(RamariaError::validation(format!(
                    "{date}: 会话消息条数不符（语料 {} 条 / 库内总数 {} 条）",
                    session.messages.len(),
                    page.total
                )));
            }
            for (artifact, message) in pair_session(session, &page.messages)? {
                artifacts.entry(artifact).or_default().push(message);
            }
        }

        map.samples.insert(sample.user_key.clone(), artifacts);
    }

    Ok(map)
}

/// 库内会话外部标识 → 会话 id（一次列全，按标识建索引）。
async fn session_index(engine: &Engine) -> RamariaResult<HashMap<String, Uuid>> {
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await?;
    Ok(page
        .items
        .into_iter()
        .filter_map(|session| {
            session
                .external_ref
                .map(|reference| (reference, session.id))
        })
        .collect())
}

// =========================================================
// 读写
// =========================================================

/// 写出映射文件（pretty JSON + 末尾换行；先临时文件再改名）。
///
/// 参数:
/// - `dir`: 档位产物目录（须已存在）；
/// - `map`: 映射数据。
///
/// 返回:
/// - 成功时返回写入的文件路径；序列化或 I/O 失败返回对应错误。
pub fn write(dir: &Path, map: &ArtifactMap) -> RamariaResult<PathBuf> {
    let mut text =
        serde_json::to_string_pretty(map).map_err(|error| RamariaError::Serialization {
            context: format!("序列化 artifact 映射失败: {}", map.tier_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;
    text.push('\n');

    let path = dir.join(ARTIFACTS_FILE);
    let temp = dir.join(format!("{ARTIFACTS_FILE}.tmp"));
    std::fs::write(&temp, text).map_err(|error| {
        RamariaError::io(
            format!("写入 artifact 映射临时文件失败: {}", temp.display()),
            Some(error),
        )
    })?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        RamariaError::io(
            format!("替换 artifact 映射失败: {}", path.display()),
            Some(error),
        )
    })?;
    Ok(path)
}

/// 读取映射文件；文件不存在返回 `None`。
///
/// 参数:
/// - `dir`: 档位产物目录（目录名即档位标识）。
///
/// 返回:
/// - 存在时返回通过类型 / 版本 / 档位校验的映射；
/// - 不存在返回 `None`；不自洽返回 `Validation`，文案带文件路径。
///
/// 说明: 不校验语料摘要——调用方按自己手上的口径决定是否再比一次。
pub fn load(dir: &Path) -> RamariaResult<Option<ArtifactMap>> {
    let path = dir.join(ARTIFACTS_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(
            format!("读取 artifact 映射失败: {}", path.display()),
            Some(error),
        )
    })?;
    let map: ArtifactMap = serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!(
            "解析 artifact 映射失败: {}: {error}",
            path.display()
        ))
    })?;
    check_shape(&map, dir, &path)?;
    Ok(Some(map))
}

/// 读取映射文件并核对语料摘要。
///
/// 参数:
/// - `dir`: 档位产物目录（目录名即档位标识）；
/// - `corpus_sha256`: 期望的语料文件摘要。
///
/// 返回:
/// - 全部校验通过时返回映射；
/// - 文件缺失、类型 / 版本 / 档位 / 摘要不一致返回 `Validation`，文案带路径。
pub fn read(dir: &Path, corpus_sha256: &str) -> RamariaResult<ArtifactMap> {
    let path = dir.join(ARTIFACTS_FILE);
    let map = load(dir)?.ok_or_else(|| {
        RamariaError::validation(format!("artifact 映射不存在: {}", path.display()))
    })?;
    if map.corpus.sha256 != corpus_sha256 {
        return Err(RamariaError::validation(format!(
            "artifact 映射的语料摘要不符: 期望 {corpus_sha256}，实际 {}（{}）",
            map.corpus.sha256,
            path.display()
        )));
    }
    Ok(map)
}

/// 产物类型、schema 版本与档位目录名三项自洽校验。
fn check_shape(map: &ArtifactMap, dir: &Path, path: &Path) -> RamariaResult<()> {
    if map.artifact_kind != ARTIFACT_KIND {
        return Err(RamariaError::validation(format!(
            "artifact 映射类型不符: 期望 {ARTIFACT_KIND}，实际 {}（{}）",
            map.artifact_kind,
            path.display()
        )));
    }
    if map.schema_version != ARTIFACT_SCHEMA_VERSION {
        return Err(RamariaError::validation(format!(
            "artifact 映射版本不符: 期望 {ARTIFACT_SCHEMA_VERSION}，实际 {}（{}）",
            map.schema_version,
            path.display()
        )));
    }
    let tier_dir = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            RamariaError::validation(format!("无法确定档位目录名: {}", dir.display()))
        })?;
    if tier_dir != map.tier_id {
        return Err(RamariaError::validation(format!(
            "档位目录名与 artifact 映射不一致: 目录 {tier_dir}，映射 {}（{}）",
            map.tier_id,
            path.display()
        )));
    }
    Ok(())
}
