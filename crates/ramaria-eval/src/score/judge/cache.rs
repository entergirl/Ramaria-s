//! crates/ramaria-eval/src/score/judge/cache.rs - Ramaria 评测判分缓存
//!
//! 设计特点:
//! - 一行一条：与逐题产物同格式，追加写，便于人工查看与手动清理
//! - 键是输入摘要：模板摘要 + 模型 + 任务 + 题目 + 条件 + 回复摘要，同输入必命中
//! - 命中即零模型调用：同一运行重复判分不产生任何请求，产物逐字节一致
//! - 坏行报错带行号：缓存被手工改坏时立刻暴露，不静默丢历史结果
//! - 只写全部票面有效的题目：任一票失败整题不写，下次判分重跑整题

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::JudgeValue;

/// 判分缓存文件名（判分产物目录下）。
pub const CACHE_FILE: &str = "judge_cache.jsonl";

/// 键字段分隔符（单元分隔符，不会出现在标识与摘要里）。
const KEY_SEPARATOR: char = '\u{1f}';

/// 一条判分缓存。
///
/// 字段约定:
/// - `key`: 缓存键（十六进制小写 sha256）；
/// - `model` / `task` / `question_id` / `condition_id`: 键的可见部分，便于排查；
/// - `template_sha256`: 本次使用模板的摘要；
/// - `votes`: 三票票面（无效票为 null）；
/// - `value`: 多数结果；判分失败为 null；
/// - `disagreements` / `units`: 分歧单位数与已判单位数（用于条件分歧率）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeCacheEntry {
    pub key: String,
    pub model: String,
    pub task: String,
    pub question_id: String,
    pub condition_id: String,
    pub template_sha256: String,
    pub votes: Vec<Option<JudgeValue>>,
    pub value: Option<JudgeValue>,
    pub disagreements: usize,
    pub units: usize,
}

/// 判分缓存（内存索引 + 追加落盘）。
///
/// 字段约定:
/// - `path`: 缓存文件路径；
/// - `entries`: 键 → 条目，启动时一次性载入。
#[derive(Debug, Default)]
pub struct JudgeCache {
    path: PathBuf,
    entries: HashMap<String, JudgeCacheEntry>,
}

impl JudgeCache {
    /// 读取缓存文件；文件不存在按空缓存处理。
    ///
    /// 参数:
    /// - `path`: 缓存文件路径。
    ///
    /// 返回:
    /// - 成功时返回已载入的缓存；
    /// - 读取失败返回 `Io`；坏行返回 `Validation`，文案带行号。
    pub fn load(path: PathBuf) -> RamariaResult<Self> {
        if !path.is_file() {
            return Ok(Self {
                path,
                entries: HashMap::new(),
            });
        }
        let text = std::fs::read_to_string(&path).map_err(|error| {
            RamariaError::io(format!("读取判分缓存失败: {}", path.display()), Some(error))
        })?;
        let mut entries = HashMap::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: JudgeCacheEntry = serde_json::from_str(line).map_err(|error| {
                RamariaError::validation(format!(
                    "解析判分缓存失败: {} 第 {} 行: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
            entries.insert(entry.key.clone(), entry);
        }
        Ok(Self { path, entries })
    }

    /// 按键取缓存条目。
    pub fn get(&self, key: &str) -> Option<&JudgeCacheEntry> {
        self.entries.get(key)
    }

    /// 追加一条缓存（同键已存在时不再落盘）。
    ///
    /// 返回:
    /// - 成功时条目同时进入内存索引；目录创建 / 写入失败返回 `Io`。
    pub fn insert(&mut self, entry: JudgeCacheEntry) -> RamariaResult<()> {
        if self.entries.contains_key(&entry.key) {
            return Ok(());
        }
        let line = serde_json::to_string(&entry).map_err(|error| RamariaError::Serialization {
            context: format!("序列化判分缓存失败: {}", entry.question_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                RamariaError::io(
                    format!("创建判分缓存目录失败: {}", parent.display()),
                    Some(error),
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| {
                RamariaError::io(
                    format!("打开判分缓存失败: {}", self.path.display()),
                    Some(error),
                )
            })?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
            .map_err(|error| {
                RamariaError::io(
                    format!("写入判分缓存失败: {}", self.path.display()),
                    Some(error),
                )
            })?;
        self.entries.insert(entry.key.clone(), entry);
        Ok(())
    }
}

/// 构造缓存键。
///
/// 参数:
/// - `template_sha256`: 任务模板摘要；
/// - `model`: 判分模型标识；
/// - `task`: 任务名；
/// - `question_id` / `condition_id`: 复合键；
/// - `reply`: 待评回复原文（内部先取摘要，不进缓存文件）。
///
/// 返回:
/// - `sha256(template_sha256 | model | task | question_id | condition_id | reply_sha256)` 的十六进制小写。
pub fn key(
    template_sha256: &str,
    model: &str,
    task: &str,
    question_id: &str,
    condition_id: &str,
    reply: &str,
) -> String {
    let reply_sha256 = format!("{:x}", Sha256::digest(reply.as_bytes()));
    let joined = [
        template_sha256,
        model,
        task,
        question_id,
        condition_id,
        reply_sha256.as_str(),
    ]
    .join(&KEY_SEPARATOR.to_string());
    format!("{:x}", Sha256::digest(joined.as_bytes()))
}
