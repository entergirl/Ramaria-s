//! crates/ramaria-eval/src/tui/console/review/index.rs - Ramaria 审查屏运行索引
//!
//! 设计特点:
//! - 按字节偏移建索引：扫描只取每行头部的复合键，翻页时再按偏移读整行
//! - 头部扫描不整行反序列化：只识别顶层两个键，字符串转义按 JSON 规则跳过
//! - 同一复合键后行覆盖前行：以最后一次写出为准，题单按最后出现位置排序
//! - 判分产物可选：优先读 `score/scored.jsonl`，缺失时逐题返回 `None`
//! - 条件计数取 `run.json`，目录里缺条件时不凭逐题产物补造条件

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::run::{ITEMS_FILE, RUN_ARTIFACT_KIND, RUN_FILE, RunItemRecord, RunState};
use crate::score::{SCORE_DIR, SCORE_FILE, SCORED_FILE, ScoredItem};

#[cfg(test)]
mod tests;

// =========================================================
// 索引结构
// =========================================================

/// 一个条件的题量与计数（来自 `run.json`）。
///
/// 字段约定:
/// - `id`: 条件标识；
/// - `total` / `completed` / `failed`: 计划题数与互斥计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConditionEntry {
    pub(crate) id: String,
    pub(crate) total: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
}

/// 一道题在逐题产物里的位置。
///
/// 字段约定:
/// - `condition_id` / `question_id`: 复合键；
/// - `offset`: 该行在 `items.jsonl` 的起始字节偏移；
/// - `order`: 最后一行出现顺序（同键多行时用于排序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemRef {
    pub(crate) condition_id: String,
    pub(crate) question_id: String,
    pub(crate) offset: u64,
    pub(crate) order: u64,
}

/// 一次运行的只读索引。
///
/// 字段约定:
/// - `run_id`: 运行标识；
/// - `items_path` / `scored_path`: 逐题产物与判分逐题产物路径；
/// - `conditions`: 条件计数（顺序取 `run.json`）；
/// - `items`: 按最后出现顺序排列的题目位置；
/// - `by_condition`: 条件标识 → 题单位置下标；
/// - `scored_offsets`: 复合键 → `scored.jsonl` 行偏移（无判分产物时为空）。
#[derive(Debug)]
pub(crate) struct ReviewIndex {
    pub(crate) run_id: String,
    pub(crate) items_path: PathBuf,
    pub(crate) scored_path: PathBuf,
    pub(crate) conditions: Vec<ConditionEntry>,
    pub(crate) items: Vec<ItemRef>,
    by_condition: BTreeMap<String, Vec<usize>>,
    scored_offsets: HashMap<(String, String), u64>,
}

impl ReviewIndex {
    /// 扫描运行根建立索引。
    ///
    /// 参数:
    /// - `root`: 运行根目录（含 `run.json`）。
    ///
    /// 返回:
    /// - 成功时只读头部建好题单与判分偏移表；
    /// - `run.json` 缺失、不可解析或不是评测运行产物返回 `Validation`（文案带路径）；
    /// - `items.jsonl` 坏行、`scored.jsonl` 坏行返回 `Validation`（文案带行号）。
    ///
    /// 说明: 逐题产物缺失（尚未跑出第一题）视为空题单，不报错。
    pub(crate) fn build(root: &Path) -> RamariaResult<Self> {
        let state = read_state(root)?;
        let items_path = root.join(ITEMS_FILE);
        let (items, by_condition) = scan_items_index(&items_path)?;
        let scored_path = root.join(SCORE_DIR).join(SCORED_FILE);
        let scored_offsets = scan_scored_index(&scored_path)?;
        Ok(Self {
            run_id: state.run_id,
            items_path,
            scored_path,
            conditions: state
                .conditions
                .into_iter()
                .map(|entry| ConditionEntry {
                    id: entry.id,
                    total: entry.total,
                    completed: entry.completed,
                    failed: entry.failed,
                })
                .collect(),
            items,
            by_condition,
            scored_offsets,
        })
    }

    /// 条件列表。
    pub(crate) fn conditions(&self) -> &[ConditionEntry] {
        &self.conditions
    }

    /// 某条件下的题单位置下标；未登记条件返回空切片。
    pub(crate) fn questions(&self, condition_id: &str) -> &[usize] {
        self.by_condition
            .get(condition_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 题单位置条目。
    pub(crate) fn item(&self, index: usize) -> Option<&ItemRef> {
        self.items.get(index)
    }

    /// 是否有判分逐题产物。
    pub(crate) fn has_scored_file(&self) -> bool {
        self.scored_path.is_file()
    }

    /// 按题单位置读取一题完整记录。
    ///
    /// 参数:
    /// - `index`: `items` 下标。
    ///
    /// 返回:
    /// - 成功时返回该行反序列化结果；
    /// - 下标越界、偏移失效或行不可解析返回 `Validation`。
    pub(crate) fn read_item(&self, index: usize) -> RamariaResult<RunItemRecord> {
        let entry = self.items.get(index).ok_or_else(|| {
            RamariaError::validation(format!("题目下标越界: {index}（共 {}）", self.items.len()))
        })?;
        let line = read_at(&self.items_path, entry.offset, index)?;
        serde_json::from_str(&line).map_err(|error| {
            RamariaError::validation(format!(
                "解析逐题产物失败: {} 第 {} 条（{}，offset {}）: {error}",
                self.items_path.display(),
                index + 1,
                entry.question_id,
                entry.offset
            ))
        })
    }

    /// 按题单位置读取判分结果。
    ///
    /// 参数:
    /// - `index`: `items` 下标。
    ///
    /// 返回:
    /// - 有判分产且命中时返回 `Some`；
    /// - 无判分产物或该题未判时返回 `None`；
    /// - 下标越界或偏移失效返回 `Validation`。
    pub(crate) fn read_scored(&self, index: usize) -> RamariaResult<Option<ScoredItem>> {
        let entry = self.items.get(index).ok_or_else(|| {
            RamariaError::validation(format!("题目下标越界: {index}（共 {}）", self.items.len()))
        })?;
        let key = (entry.condition_id.clone(), entry.question_id.clone());
        let Some(offset) = self.scored_offsets.get(&key).copied() else {
            return Ok(None);
        };
        let line = read_at(&self.scored_path, offset, index)?;
        let scored: ScoredItem = serde_json::from_str(&line).map_err(|error| {
            RamariaError::validation(format!(
                "解析判分产物失败: {} 第 {} 条（{}，offset {offset}）: {error}",
                self.scored_path.display(),
                index + 1,
                entry.question_id
            ))
        })?;
        Ok(Some(scored))
    }
}

// =========================================================
// 状态与扫描
// =========================================================

/// 读取并校验 `run.json`。
fn read_state(root: &Path) -> RamariaResult<RunState> {
    let path = root.join(RUN_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(
            format!("读取运行状态失败: {}（不是运行根目录？）", path.display()),
            Some(error),
        )
    })?;
    let state: RunState = serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!("解析运行状态失败: {}: {error}", path.display()))
    })?;
    if state.artifact_kind != RUN_ARTIFACT_KIND {
        return Err(RamariaError::validation(format!(
            "不是评测运行产物: {}（artifact_kind = {}）",
            path.display(),
            state.artifact_kind
        )));
    }
    Ok(state)
}

/// 扫描逐题产物建立题单；文件缺失视为空题单。
fn scan_items_index(path: &Path) -> RamariaResult<(Vec<ItemRef>, ConditionMap)> {
    let mut items: Vec<ItemRef> = Vec::new();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();

    scan_lines(path, |number, offset, line| {
        let (question_id, condition_id) = head_ids(line).ok_or_else(|| {
            RamariaError::validation(format!(
                "解析逐题产物失败: {} 第 {number} 行: 缺少 question_id / condition_id",
                path.display()
            ))
        })?;
        let key = (condition_id.clone(), question_id.clone());
        let order = number as u64;
        match seen.get(&key).copied() {
            Some(position) => {
                // 追加写：同键后行覆盖前行，排序按最后一次出现
                items[position].offset = offset;
                items[position].order = order;
            }
            None => {
                seen.insert(key, items.len());
                items.push(ItemRef {
                    condition_id,
                    question_id,
                    offset,
                    order,
                });
            }
        }
        Ok(())
    })?;

    items.sort_by_key(|entry| entry.order);
    let mut by_condition: ConditionMap = BTreeMap::new();
    for (index, entry) in items.iter().enumerate() {
        by_condition
            .entry(entry.condition_id.clone())
            .or_default()
            .push(index);
    }
    Ok((items, by_condition))
}

/// 扫描判分逐题产物建立偏移表；文件缺失视为未判分。
fn scan_scored_index(path: &Path) -> RamariaResult<HashMap<(String, String), u64>> {
    let mut offsets = HashMap::new();
    scan_lines(path, |number, offset, line| {
        let (question_id, condition_id) = head_ids(line).ok_or_else(|| {
            RamariaError::validation(format!(
                "解析判分产物失败: {} 第 {number} 行: 缺少 question_id / condition_id",
                path.display()
            ))
        })?;
        offsets.insert((condition_id, question_id), offset);
        Ok(())
    })?;
    Ok(offsets)
}

/// 逐行读取并回调每行起始偏移与行号；文件缺失直接返回。
///
/// 说明: 逐行回调以避免把整份产物读进内存（6300 题约 118MB）。
fn scan_lines<F>(path: &Path, mut visit: F) -> RamariaResult<()>
where
    F: FnMut(usize, u64, &str) -> RamariaResult<()>,
{
    if !path.is_file() {
        return Ok(());
    }
    let file = File::open(path).map_err(|error| {
        RamariaError::io(format!("打开产物失败: {}", path.display()), Some(error))
    })?;
    let mut reader = BufReader::new(file);
    let mut offset = 0u64;
    let mut number = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|error| {
            RamariaError::io(format!("读取产物失败: {}", path.display()), Some(error))
        })?;
        if read == 0 {
            break;
        }
        number += 1;
        let start = offset;
        offset += read as u64;
        let text = line.trim_end_matches(['\r', '\n']);
        if text.trim().is_empty() {
            continue;
        }
        visit(number, start, text)?;
    }
    Ok(())
}

/// 条件标识 → 题单位置下标。
type ConditionMap = BTreeMap<String, Vec<usize>>;

/// 从一行 JSON 顶层取 `question_id` 与 `condition_id`。
///
/// 参数:
/// - `line`: 单行 JSON 文本。
///
/// 返回:
/// - 两个键都存在且都是字符串时返回 `(question_id, condition_id)`；
/// - 任一缺失或格式不符返回 `None`。
///
/// 说明: 只扫描顶层键值，字符串按转义规则跳过，不反序列化其余字段。
fn head_ids(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    let mut depth = 0usize;
    let mut question: Option<String> = None;
    let mut condition: Option<String> = None;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let (start, end, next) = scan_string(bytes, index)?;
                index = next;
                let key = &bytes[start..end];
                if depth != 1 || !is_target_key(key) {
                    continue;
                }
                let cursor = skip_space(bytes, skip_colon(bytes, skip_space(bytes, index))?);
                if bytes.get(cursor) != Some(&b'"') {
                    continue;
                }
                let (value_start, value_end, value_next) = scan_string(bytes, cursor)?;
                let value = decode_string(bytes, value_start, value_end);
                if key == b"question_id" {
                    question = Some(value);
                } else {
                    condition = Some(value);
                }
                index = value_next;
            }
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            _ => index += 1,
        }
        if question.is_some() && condition.is_some() {
            break;
        }
    }

    Some((question?, condition?))
}

/// 是否为索引需要的两个键之一。
fn is_target_key(key: &[u8]) -> bool {
    key == b"question_id" || key == b"condition_id"
}

/// 跳过空白，返回下一个非空白字节位置。
fn skip_space(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

/// 跳过一个冒号，返回其后位置。
fn skip_colon(bytes: &[u8], index: usize) -> Option<usize> {
    if bytes.get(index) == Some(&b':') {
        Some(index + 1)
    } else {
        None
    }
}

/// 扫描一个 JSON 字符串字面量。
///
/// 参数:
/// - `bytes`: 整行字节；
/// - `start`: 开引号位置。
///
/// 返回:
/// - `(内容起点, 闭引号位置, 闭引号后一位)`；未闭合返回 `None`。
fn scan_string(bytes: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            // 转义后的字符一律跳过：\'uXXXX\' 的十六进制位不会影响闭合判断
            b'\\' => index += 2,
            b'"' => return Some((start + 1, index, index + 1)),
            _ => index += 1,
        }
    }
    None
}

/// 按 JSON 规则解码字符串内容。
fn decode_string(bytes: &[u8], start: usize, end: usize) -> String {
    let raw = String::from_utf8_lossy(&bytes[start..end]);
    if !raw.contains('\\') {
        return raw.into_owned();
    }
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some('/') => output.push('/'),
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('b') => output.push('\u{8}'),
            Some('f') => output.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let code = u32::from_str_radix(&hex, 16).unwrap_or('\u{fffd}' as u32);
                output.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            Some(other) => output.push(other),
            None => {}
        }
    }
    output
}

/// 从文件指定偏移读一行。
fn read_at(path: &Path, offset: u64, index: usize) -> RamariaResult<String> {
    let file = File::open(path).map_err(|error| {
        RamariaError::io(format!("打开产物失败: {}", path.display()), Some(error))
    })?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset)).map_err(|error| {
        RamariaError::io(format!("定位产物失败: {}", path.display()), Some(error))
    })?;
    let mut line = String::new();
    let read = reader.read_line(&mut line).map_err(|error| {
        RamariaError::io(format!("读取产物失败: {}", path.display()), Some(error))
    })?;
    if read == 0 {
        return Err(RamariaError::validation(format!(
            "产物偏移已失效或文件被截断: {}（第 {} 条，offset {offset}）",
            path.display(),
            index + 1
        )));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// 是否已有判分产物：条件汇总或逐题判分行任一存在即算已判分。
pub(crate) fn score_present(root: &Path) -> bool {
    let dir = root.join(SCORE_DIR);
    dir.join(SCORE_FILE).is_file() || dir.join(SCORED_FILE).is_file()
}
