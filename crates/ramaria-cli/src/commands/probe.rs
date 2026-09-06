//! crates/ramaria-cli/src/commands/probe.rs - 探针 CLI 命令（probe build / probe run）
//!
//! 设计特点:
//! - `probe build`：从导入数据自动构建测试集（问题 × 参数档位组合，seed 固定可复跑），
//!   输出结构化 JSON 数据集；`dataset` 保留为 alias。
//! - `probe run`：按档位批量跑对话管线，结构化输出（档位 → 输出 → 指标），
//!   供 v1.6 T2 自动评分（evaluate/report）与 v1.7 T3 正式评估复用同一工具链。
//! - 探针规模：3 维（语气模仿 tone / 事实记忆 fact / 情感表达 emotion）× 每维 10 题，
//!   正式评估可通过 `--questions-per-dim` 扩大（v1.7 ≥ 30 题）。
//! - 档位为「代表配对」：baseline + 每次只动一个参数，
//!   θ_gap / 条数上限 / top_k 各 2 档，便于归因单参数影响。
//! - 静默降级：无真实数据 / 数据源查询失败 / 文件解析失败 → 内置测试夹具数据兜底
//!   （不向用户抛错，记 warn 后继续，M2 验收要求）。
//! - 确定性抽样：内置 xorshift64* 伪随机（无外部依赖），seed 相同则测试集完全一致。
//! - 隐私红线：数据集仅含问题与参考文本（评估所需），不包含完整对话原文；
//!   日志不记录完整问题与回复（用 id / 长度代替）。

use std::sync::Arc;

use ramaria_core::types::now_ms;

mod dataset;
mod evaluate;
mod report;
mod run;
mod types;

// 常量与纯数据类型下沉至 types.rs，对外名称在此 re-export（引用路径保持不变）。
pub use types::*;

// 测试集 dataset 构建与内置夹具兜底下沉至 dataset.rs；
// 根模块经 `pub use` 沿用原名称（probe build / scoring 调用点不变）。
pub use dataset::{
    build_dataset, build_from_file, build_from_fixture, default_variants, fixture_emotion_pairs,
    fixture_fact_events, fixture_tone_pairs, has_emotion_cue, has_negative_cue, has_positive_cue,
    run_build, sample_with_fallback, select_target_persona,
};

// 档位实验 run 族下沉至 run.rs；对外 API（build_experiment / build_experiment_with_repeat）
// 经 `pub use` 沿用原路径（probe build / run 调用点不变）。
pub use run::{build_experiment, build_experiment_with_repeat};

// run.rs 中被根模块 run 入口复用的内部函数，经 `use` 拉入根命名空间。
use run::run_experiment;

// evaluate 自动评分族下沉至 evaluate.rs；run 入口经 `use` 沿用原名称。
use evaluate::run_evaluate;

// report 对比报告族下沉至 report.rs；run 入口经 `use` 沿用原名称。
use report::run_report;

// report 数据容器对外保持原路径（probe::ProbeReport 等），在此 re-export。
pub use report::{
    AblationComparisonRow, AblationReport, CalibrationResult, DimensionRecommendation,
    KnowledgeQualityReport, ManualScore, ProbeReport, Recommendation, VariantAuxMetrics,
    VariantReportRow,
};

/// probe 命令入口（probe 不需要交互确认，yes 参数供线上 provider 隐私确认透传）。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: ProbeCmd, yes: bool) -> anyhow::Result<()> {
    match cmd {
        ProbeCmd::Build {
            persona,
            questions_per_dim,
            seed,
            source,
            output,
            json,
        } => run_build(app, persona, questions_per_dim, seed, source, output, json).await,
        ProbeCmd::Run {
            dataset,
            variants,
            limit,
            rebuild_utt,
            repeat,
            output,
            json,
        } => {
            run_experiment(
                app,
                dataset,
                variants,
                limit,
                rebuild_utt,
                repeat,
                output,
                json,
                yes,
            )
            .await
        }
        ProbeCmd::Evaluate {
            results,
            dataset,
            variants,
            output,
            no_tone_judge,
            json,
        } => {
            run_evaluate(
                app,
                &results,
                dataset.as_deref(),
                variants.as_deref(),
                output.as_deref(),
                no_tone_judge,
                json,
            )
            .await
        }
        ProbeCmd::Report {
            results,
            evaluation,
            calibration,
            output,
            ablation,
            json,
        } => {
            run_report(
                app,
                &results,
                evaluation.as_deref(),
                calibration.as_deref(),
                output.as_deref(),
                ablation,
                json,
            )
            .await
        }
    }
}

// =========================================================
// 时间辅助
// =========================================================

/// 当前时间（ISO-8601 UTC，M1 约定）。
fn now_iso8601() -> String {
    crate::util::format_timestamp_iso(now_ms())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

// =========================================================
// 确定性伪随机（xorshift64*，无外部依赖，seed 固定可复跑）
// =========================================================

/// 确定性伪随机数生成器。
///
/// 职责:
/// - 为测试集抽样提供可复现的随机序列（同 seed → 同测试集）。
/// - 使用 xorshift64*（无外部 crate 依赖，实现短、可单测）。
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    /// 创建生成器（seed=0 时使用固定非零种子，避免全零状态退化）。
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// 下一个 u64 随机数（xorshift64*）。
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 返回 `[0, bound)` 内的随机数。
    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// Fisher-Yates 洗牌（同 seed 同顺序，保证抽样可复跑）。
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.next_usize(i + 1);
            items.swap(i, j);
        }
    }
}

// =========================================================
// 单元测试（独立文件 tests.rs）
// =========================================================

#[cfg(test)]
mod tests;
