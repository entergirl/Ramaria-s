//! crates/ramaria-service/tests/parity/support/snapshot.rs - 规范化快照与对照断言
//!
//! 设计特点:
//! - 规范化：对象键排序、整数保持原样、浮点统一保留 6 位小数，消除序列化细节差异；
//!   id / 时间戳等易变字段由快照构造方决定是否落盘（本基建不做事后擦除）
//! - 差异可定位：对照不等价时输出逐路径差异（`$.items[0].score: 左 ... != 右 ...`），
//!   附两侧快照（超长自动截断）与排查提示
//! - 一类断言两种语义：`assert_parity`（两侧产出等价）与 `assert_stable`（同实现重复执行确定性），
//!   共用同一比较与报告实现
//! - 失败即 panic：断言失败绝不被 `?` 吞掉，`cargo test` 输出即完整诊断报告
//! - 隐私：报告只输出用例产出的结构化快照，不含原始 Prompt 与消息全文（由快照构造方保证）

use std::collections::BTreeSet;

use serde_json::Value;

/// 单次差异报告的最大条目数（避免超大结构刷屏，超限时提示截断）。
const MAX_DIFFERENCES: usize = 24;

/// 报告中单侧快照的最大字符数（超出截断并提示完整内容位置）。
const MAX_SNAPSHOT_CHARS: usize = 3_000;

/// 规范化快照：标签 + 稳定 JSON 值。
///
/// 职责:
/// - 承载一次场景执行的规范化输出，作为对照与基线冻结的比对单元；
/// - 构造时即完成规范化（[`canon`]），保证比对不受浮点序列化细节影响。
///
/// 字段约定:
/// - `label`: 场景名（同时作为 golden 文件名）；
/// - `value`: 规范化后的 JSON 值。
#[derive(Debug, Clone)]
pub struct Snapshot {
    label: String,
    value: Value,
}

impl Snapshot {
    /// 构造快照（自动规范化）。
    pub fn new(label: impl Into<String>, value: Value) -> Self {
        Self {
            label: label.into(),
            value: canon(value),
        }
    }

    /// 场景名（golden 文件名来源）。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// 规范化后的 JSON 值。
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// 美化序列化（用于报告与基线文件内容）。
    pub fn to_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.value)
            .unwrap_or_else(|e| format!("<快照序列化失败：{e}>"))
    }
}

// =========================================================
// 规范化
// =========================================================

/// 规范化 JSON 值：对象键排序、浮点保留 6 位小数、数组保持顺序。
///
/// 说明:
/// - 整数（i64 / u64）保持原样，避免 `1` 变成 `1.0` 影响可读性；
/// - 无法表示为 f64 的数值按 `null` 处理（用例快照不应出现此类值）。
pub fn canon(value: Value) -> Value {
    match value {
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                Value::Number(number)
            } else {
                number.as_f64().and_then(canon_float).unwrap_or(Value::Null)
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canon).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, item)| (key, canon(item)))
                .collect(),
        ),
        other => other,
    }
}

/// 浮点规范化：四舍五入到 6 位小数（足够区分行为差异，又屏蔽末位抖动）。
fn canon_float(value: f64) -> Option<Value> {
    let rounded = (value * 1_000_000.0).round() / 1_000_000.0;
    serde_json::Number::from_f64(rounded).map(Value::Number)
}

// =========================================================
// 差异计算
// =========================================================

/// 逐个路径比较两个 JSON 值，返回差异描述（上限 [`MAX_DIFFERENCES`] 条）。
pub fn diff(left: &Value, right: &Value) -> Vec<String> {
    let mut differences = Vec::new();
    diff_at("$", left, right, &mut differences);
    differences
}

/// 递归比较单个路径。
fn diff_at(path: &str, left: &Value, right: &Value, out: &mut Vec<String>) {
    if out.len() >= MAX_DIFFERENCES {
        return;
    }
    match (left, right) {
        (Value::Object(left_fields), Value::Object(right_fields)) => {
            let keys: BTreeSet<&String> = left_fields.keys().chain(right_fields.keys()).collect();
            for key in keys {
                let child = format!("{path}.{key}");
                match (left_fields.get(key), right_fields.get(key)) {
                    (Some(left_item), Some(right_item)) => {
                        diff_at(&child, left_item, right_item, out)
                    }
                    (Some(left_item), None) => {
                        push(out, format!("{child}: 仅左侧存在 {}", summarize(left_item)))
                    }
                    (None, Some(right_item)) => push(
                        out,
                        format!("{child}: 仅右侧存在 {}", summarize(right_item)),
                    ),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(left_items), Value::Array(right_items)) => {
            if left_items.len() != right_items.len() {
                push(
                    out,
                    format!(
                        "{path}: 长度不同（左 {} / 右 {}）",
                        left_items.len(),
                        right_items.len()
                    ),
                );
            }
            for index in 0..left_items.len().min(right_items.len()) {
                diff_at(
                    &format!("{path}[{index}]"),
                    &left_items[index],
                    &right_items[index],
                    out,
                );
            }
        }
        _ => {
            if left != right {
                push(
                    out,
                    format!("{path}: 左 {} != 右 {}", summarize(left), summarize(right)),
                );
            }
        }
    }
}

/// 追加差异条目（达上限后不再收集）。
fn push(out: &mut Vec<String>, item: String) {
    if out.len() < MAX_DIFFERENCES {
        out.push(item);
    }
}

/// 单值摘要：序列化后按字符截断（中文按字符计数，避免切坏 UTF-8）。
fn summarize(value: &Value) -> String {
    truncate(&value.to_string(), 120)
}

/// 按字符截断字符串，超出部分以省略号提示。
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…")
}

// =========================================================
// 对照断言
// =========================================================

/// 两侧等价断言：左侧与右侧快照必须一致，否则 panic 并输出差异报告。
///
/// 用法:
/// - 同一 fixture 下两份快照（不同实现，或同一实现的两份独立产出）送入本函数比对；
/// - 侧重"两侧产出是否等价"，与 [`assert_stable`] 共用比较与报告实现。
pub fn assert_parity(label: &str, left: &Snapshot, right: &Snapshot) {
    assert_equal(
        label,
        left,
        right,
        "左侧",
        "右侧",
        "两侧在同一 fixture 上输出不一致：以差异路径定位漂移点；\n\
         差异属预期时先更新快照期望与基线，否则修复产出侧后重跑。",
    );
}

/// 同实现确定性断言：同一场景在两个隔离环境重复执行，输出必须一致。
///
/// 用法:
/// - 验证 fixture 与 mock 的确定性（跨环境、跨运行无随机与时钟漂移）；
/// - 作为 `assert_parity` 的先行检查：确定性不成立时对照结果无意义。
pub fn assert_stable(label: &str, first: &Snapshot, replay: &Snapshot) {
    assert_equal(
        label,
        first,
        replay,
        "首次执行",
        "重复执行",
        "同一实现在两个隔离环境上输出不一致：检查 fixture 是否引入随机值 /\
         绝对时间 / 未固定顺序，或 mock 是否失去确定性。",
    );
}

/// 生成差异报告：等价时返回 `None`，不等价时返回完整报告文本（含逐路径差异与两侧快照）。
///
/// 用法:
/// - 对照断言与基线比对共用同一报告实现（[`assert_parity`] / golden 基线比对）；
/// - 参数 `left_title` / `right_title` 用于在报告中标注两侧身份（如"冻结基线 / 本次执行"）。
///
/// 参数:
/// - `label`: 场景名（进入报告首行与日志字段）。
/// - `left` / `right`: 待比较的两份快照。
/// - `left_title` / `right_title`: 两侧身份标注。
/// - `hint`: 排查提示（追加在报告末尾）。
pub fn diff_report(
    label: &str,
    left: &Snapshot,
    right: &Snapshot,
    left_title: &str,
    right_title: &str,
    hint: &str,
) -> Option<String> {
    let differences = diff(left.value(), right.value());
    if differences.is_empty() {
        tracing::info!(
            label,
            left = left.label(),
            right = right.label(),
            "快照等价"
        );
        return None;
    }

    tracing::error!(
        label,
        differences = differences.len(),
        left = left.label(),
        right = right.label(),
        "快照不等价"
    );

    let listed = differences
        .iter()
        .map(|item| format!("  - {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut report = format!(
        "快照不等价：{label}\n\n差异（最多 {MAX_DIFFERENCES} 条）:\n{listed}\n\n\
         左侧（{left_title}）:\n{}\n\n右侧（{right_title}）:\n{}\n\n排查提示: {hint}",
        truncate(&left.to_pretty(), MAX_SNAPSHOT_CHARS),
        truncate(&right.to_pretty(), MAX_SNAPSHOT_CHARS),
    );
    if differences.len() >= MAX_DIFFERENCES {
        report.push_str("\n（差异列表已达上限，仅显示前若干条）");
    }
    Some(report)
}

/// 断言两个快照等价（共享实现），不等价时 panic 并输出差异报告。
fn assert_equal(
    label: &str,
    left: &Snapshot,
    right: &Snapshot,
    left_title: &str,
    right_title: &str,
    hint: &str,
) {
    if let Some(report) = diff_report(label, left, right, left_title, right_title, hint) {
        panic!("{report}");
    }
}
