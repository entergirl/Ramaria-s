//! crates/ramaria-cli/src/commands/probe/run/experiment/output.rs - 探针实验结果过滤与输出
//!
//! 设计特点:
//! - 按 --variants 过滤档位（未知 id 记 warn 跳过）。
//! - 结果 JSON 写出与文本摘要渲染。

use crate::commands::probe::types::{ProbeExperiment, ProbeVariant};
use anyhow::Context;

/// 按 --variants 过滤档位（逗号分隔；无效 id 记 warn 跳过）。
pub(crate) fn filter_variants(
    variants: &[ProbeVariant],
    filter: Option<&str>,
) -> Vec<ProbeVariant> {
    let Some(filter) = filter else {
        return variants.to_vec();
    };
    let ids: Vec<&str> = filter
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut out = Vec::new();
    for id in ids {
        match variants.iter().find(|v| v.id == id) {
            Some(v) => out.push(v.clone()),
            None => {
                tracing::warn!(variant_id = id, "probe run 忽略未知档位 id");
            }
        }
    }
    if out.is_empty() {
        tracing::warn!("probe run 档位过滤结果为空，回退为全部档位");
        variants.to_vec()
    } else {
        out
    }
}

/// 写实验结果到文件（`-` 表示 stdout，输出原始结果 JSON）。
///
/// 说明: `-` 直出 stdout（含库内原文，口径见模块头说明）。
pub(crate) fn write_experiment_file(out: &str, experiment: &ProbeExperiment) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(experiment).context("实验结果序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入实验结果失败: {out}"))?;
    }
    Ok(())
}

/// 文本模式打印实验结果摘要。
pub(crate) fn print_experiment_summary(experiment: &ProbeExperiment) {
    println!(
        "probe 实验结果: persona={} | {} 档位 | 数据集 seed={}",
        experiment.persona_uid,
        experiment.variants.len(),
        experiment.dataset_seed
    );
    for v in &experiment.variants {
        let success = v.runs.len() - v.failed_count;
        let avg_ms: u128 = if v.runs.is_empty() {
            0
        } else {
            v.runs.iter().map(|r| r.metrics.elapsed_ms).sum::<u128>() / v.runs.len() as u128
        };
        let avg_chars: usize = if v.runs.is_empty() {
            0
        } else {
            v.runs.iter().map(|r| r.metrics.reply_chars).sum::<usize>() / v.runs.len()
        };
        println!(
            "  档位 {:<14} 成功={}/{:<3} 平均回复 {} 字符 / {} ms  — {}",
            v.variant_id,
            success,
            v.runs.len(),
            avg_chars,
            avg_ms,
            v.description
        );
    }
    // 统计法（--repeat N）：展示各档位跨 N 次的均值（细目与置信区间见 --json/文件）
    if let Some(rep) = &experiment.repeat {
        println!(
            "  统计法: {} 次运行，各档位平均（细目/95% 置信区间见 --json 或 --output）",
            rep.count
        );
        for vs in &rep.per_variant {
            let n = vs.per_item.len();
            if n == 0 {
                continue;
            }
            let chars_mean: f64 =
                vs.per_item.iter().map(|s| s.reply_chars.mean).sum::<f64>() / n as f64;
            let ms_mean: f64 =
                vs.per_item.iter().map(|s| s.elapsed_ms.mean).sum::<f64>() / n as f64;
            println!(
                "    档位 {:<14} 平均回复 {:.1} 字符 / {:.1} ms（N={}）",
                vs.variant_id, chars_mean, ms_mean, rep.count
            );
        }
    }
    if experiment.utt_restored == Some(true) {
        crate::ui::info("utt 块已按基准参数复原（档位切分未留在库中）");
    }
    crate::ui::info("完整结果含每题 reply/metrics，可用 --json 或 --output 获取");
}
