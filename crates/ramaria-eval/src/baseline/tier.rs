//! crates/ramaria-eval/src/baseline/tier.rs - Ramaria 评测基线档位
//!
//! 设计特点:
//! - 五个档位一次定死：用户数、建库窗口与查询截止日都是常量，运行期不再拼装
//! - 建库窗口与查询窗口分离：消息只灌到 `memory_end`，QA 按 `query_end` 过滤
//! - `all` 只描述构建顺序与检查点：u1 / u5 从同一条全年库上取快照，不重复灌入
//! - 检查点复用建库窗口：同一次构建产出的档位只在用户数与样本数上不同
//! - 窗口判定与样本选择为纯函数，不读文件、不碰引擎

use chrono::NaiveDate;
use ramaria_core::error::{RamariaError, RamariaResult};

use crate::corpus::Sample;

/// 已登记档位标识（顺序即 `all` 的查看顺序）。
pub const TIER_IDS: [&str; 5] = ["u1-m1", "u1-q1", "u1-y1", "u5-y1", "u10-y1"];

/// 构建档位：一次建库需要的用户数、灌入窗口与查询截止日。
///
/// 字段约定:
/// - `id`: 稳定档位标识，同时是产物子目录名；
/// - `users`: 活跃记忆用户数（样本数）；
/// - `memory_start` / `memory_end`: 消息灌入窗口（含两端）；
/// - `query_end`: QA 过滤的提问截止日（含当日），晚于建库窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierSpec {
    pub id: &'static str,
    pub users: usize,
    pub memory_start: NaiveDate,
    pub memory_end: NaiveDate,
    pub query_end: NaiveDate,
}

impl TierSpec {
    /// 建库窗口天数（含两端）。
    pub fn span_days(&self) -> i64 {
        (self.memory_end - self.memory_start).num_days() + 1
    }

    /// 消息日期是否落在灌入窗口内。
    pub fn includes_date(&self, date: NaiveDate) -> bool {
        date >= self.memory_start && date <= self.memory_end
    }

    /// 提问日期是否落在查询窗口内（起点取建库起点，终点取查询截止日）。
    pub fn includes_question(&self, ask_time: NaiveDate) -> bool {
        ask_time >= self.memory_start && ask_time <= self.query_end
    }

    /// 产物基线标识：`<corpus>-<version>-<language>-<tier>`。
    pub fn baseline_id(&self, corpus: &str, version: &str, language: &str) -> String {
        format!("{corpus}-{version}-{language}-{}", self.id)
    }
}

/// 单个档位的构建检查点：灌入 `after_samples` 个样本后落一个档位产物。
///
/// 字段约定:
/// - `after_samples`: 已完成样本数（1 起）；
/// - `tier`: 该检查点对应的档位（用户数即活跃样本数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub after_samples: usize,
    pub tier: TierSpec,
}

/// 一次建库步骤：窗口 + 样本数 + 检查点。
///
/// 字段约定:
/// - `window`: 建库窗口（决定灌入哪些日期、创建哪些人格）；
/// - `sample_count`: 本次建库灌入的样本数，也是预先创建的人格数；
/// - `checkpoints`: 样本完成后的产物落点，按 `after_samples` 升序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStep {
    pub window: TierSpec,
    pub sample_count: usize,
    pub checkpoints: Vec<Checkpoint>,
}

/// 档位选择：`all` 或单个档位。
///
/// 字段约定:
/// - `All`: 按 `u1-m1`、`u1-q1`、全年（u10 一次建库）顺序执行；
/// - `One`: 只构建给定档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierSelection {
    All,
    One(TierSpec),
}

impl TierSelection {
    /// 展开为构建步骤。
    ///
    /// 返回:
    /// - 单档位：一个步骤，检查点即该档位；
    /// - `all`：三个步骤，全年步骤在第 1 / 5 / 10 个样本后各落一个检查点。
    pub fn plan(&self) -> Vec<BuildStep> {
        match self {
            Self::One(spec) => vec![single_step(*spec)],
            Self::All => vec![
                single_step(U1_M1),
                single_step(U1_Q1),
                BuildStep {
                    window: U10_Y1,
                    sample_count: U10_Y1.users,
                    checkpoints: vec![
                        Checkpoint {
                            after_samples: 1,
                            tier: U1_Y1,
                        },
                        Checkpoint {
                            after_samples: 5,
                            tier: U5_Y1,
                        },
                        Checkpoint {
                            after_samples: 10,
                            tier: U10_Y1,
                        },
                    ],
                },
            ],
        }
    }
}

/// 构建一个档位的一次建库步骤。
fn single_step(spec: TierSpec) -> BuildStep {
    BuildStep {
        window: spec,
        sample_count: spec.users,
        checkpoints: vec![Checkpoint {
            after_samples: spec.users,
            tier: spec,
        }],
    }
}

// =========================================================
// 固定档位
// =========================================================

/// 2025-12 单月。
pub const U1_M1: TierSpec = TierSpec {
    id: "u1-m1",
    users: 1,
    memory_start: fixed_date(2025, 12, 1),
    memory_end: fixed_date(2025, 12, 31),
    query_end: fixed_date(2026, 1, 15),
};

/// 2025 年第四季度。
pub const U1_Q1: TierSpec = TierSpec {
    id: "u1-q1",
    users: 1,
    memory_start: fixed_date(2025, 10, 1),
    memory_end: fixed_date(2025, 12, 31),
    query_end: fixed_date(2026, 1, 15),
};

/// 单用户全年。
pub const U1_Y1: TierSpec = TierSpec {
    id: "u1-y1",
    users: 1,
    memory_start: fixed_date(2025, 1, 1),
    memory_end: fixed_date(2025, 12, 31),
    query_end: fixed_date(2026, 1, 15),
};

/// 五用户全年。
pub const U5_Y1: TierSpec = TierSpec {
    id: "u5-y1",
    users: 5,
    memory_start: fixed_date(2025, 1, 1),
    memory_end: fixed_date(2025, 12, 31),
    query_end: fixed_date(2026, 1, 15),
};

/// 十用户全年。
pub const U10_Y1: TierSpec = TierSpec {
    id: "u10-y1",
    users: 10,
    memory_start: fixed_date(2025, 1, 1),
    memory_end: fixed_date(2025, 12, 31),
    query_end: fixed_date(2026, 1, 15),
};

/// 固定日期常量：非法日期在编译期即失败。
const fn fixed_date(year: i32, month: u32, day: u32) -> NaiveDate {
    match NaiveDate::from_ymd_opt(year, month, day) {
        Some(date) => date,
        None => panic!("评测档位日期非法"),
    }
}

// =========================================================
// 解析与样本选择
// =========================================================

/// 解析档位取值。
///
/// 参数:
/// - `raw`: 命令行传入的档位（`all` 或 `TIER_IDS` 之一，大小写不敏感）。
///
/// 返回:
/// - 命中返回档位选择；其他取值返回 `Validation`，文案列出合法取值。
pub fn parse_tier(raw: &str) -> RamariaResult<TierSelection> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "all" => Ok(TierSelection::All),
        "u1-m1" => Ok(TierSelection::One(U1_M1)),
        "u1-q1" => Ok(TierSelection::One(U1_Q1)),
        "u1-y1" => Ok(TierSelection::One(U1_Y1)),
        "u5-y1" => Ok(TierSelection::One(U5_Y1)),
        "u10-y1" => Ok(TierSelection::One(U10_Y1)),
        other => Err(RamariaError::validation(format!(
            "未知基线段位: {other}（可选: all / {}）",
            TIER_IDS.join(" / ")
        ))),
    }
}

/// 按语料顺序取前 `count` 个样本。
///
/// 参数:
/// - `samples`: 语料文件内的全部样本（保持文件顺序）；
/// - `count`: 需要的样本数。
///
/// 返回:
/// - 前 `count` 个样本的切片；样本不足返回 `Validation`。
pub fn select_samples(samples: &[Sample], count: usize) -> RamariaResult<&[Sample]> {
    if samples.len() < count {
        return Err(RamariaError::validation(format!(
            "语料样本不足: 需要 {count} 个，实际 {} 个",
            samples.len()
        )));
    }
    if count == 0 {
        return Err(RamariaError::validation("语料样本数必须大于 0"));
    }
    Ok(&samples[..count])
}

#[cfg(test)]
mod tests;
