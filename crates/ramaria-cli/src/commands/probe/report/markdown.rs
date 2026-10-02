//! crates/ramaria-cli/src/commands/probe/report/markdown.rs - 探针 report 报告渲染与输出
//!
//! 设计特点:
//! - markdown / JSON 双形态输出：写文件或直出 stdout（`-`）
//! - render_report_markdown 为纯函数，按分节渲染对比表与局限声明
//! - print_report_summary 输出运行摘要到 stderr

use anyhow::Context;

use super::knowledge_quality::KnowledgeJudgeRates;
use super::render::{AblationComparisonRow, ProbeReport};

/// 写 JSON 报告到文件。
///
/// 说明: `-` 直出 stdout（含库内原文，口径见模块头 CR-SEC-102 登记）。
pub(crate) fn write_report_json(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(report).context("报告 JSON 序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n")).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 写 markdown 报告到文件。
///
/// 说明: `-` 直出 stdout（含库内原文，口径见模块头 CR-SEC-102 登记）。
pub(crate) fn write_report_markdown(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let md = render_report_markdown(report);
    if out == "-" {
        print!("{md}");
    } else {
        std::fs::write(out, md).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 渲染 markdown 报告（档位对比表 + 定稿建议 + 校准 + 知识层质量）。
pub(crate) fn render_report_markdown(report: &ProbeReport) -> String {
    let mut md = String::new();
    md.push_str("# Ramaria 探针档位对比报告\n\n");
    md.push_str(&format!("- persona: `{}`\n", report.persona_uid));
    md.push_str(&format!("- 数据集 seed: {}\n", report.dataset_seed));
    md.push_str(&format!(
        "- 语气 judge: {} / 事实 embedding: {}\n",
        report.judge_used, report.embedding_used
    ));
    md.push_str(&format!("- 生成时间: {}\n\n", report.generated_at));

    // 档位对比表
    md.push_str("## 档位评分对比\n\n");
    md.push_str(
        "| 档位 | 事实维 | 事实(归一) | 事实(事实点) | 语气维 | 情感维 | 成功/总 | 失败 | 说明 |\n",
    );
    md.push_str("|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|------|\n");
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_norm = r
            .fact_score_norm
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_point = r
            .fact_score_point
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {}/{} | {} | {} |\n",
            r.variant_id,
            fact,
            fact_norm,
            fact_point,
            tone,
            emotion,
            r.success_count,
            r.total_count,
            r.failed_count,
            r.description.replace('|', "\\|")
        ));
    }
    md.push('\n');

    md.push_str("> 情感维为描述性指标（口径未校准），不参与层价值判定。\n\n");

    // 定稿建议
    md.push_str("## 定稿建议\n\n");
    for d in &report.recommendation.per_dimension {
        md.push_str(&format!(
            "**{}**：{}（最佳档位 {}）\n\n",
            d.dimension,
            d.reason,
            d.best_variant.as_deref().unwrap_or("—")
        ));
    }
    md.push_str(&format!("**综合**：{}\n\n", report.recommendation.overall));

    // 人工抽检校准
    if let Some(c) = &report.calibration {
        md.push_str("## 人工抽检校准\n\n");
        md.push_str(&format!(
            "- 抽检样本：{}/{}（{:.0}%）\n",
            c.sample_count,
            c.total_count,
            c.sample_rate * 100.0
        ));
        md.push_str(&format!(
            "- 同分一致性：{:.0}%\n",
            c.consistency_exact * 100.0
        ));
        md.push_str(&format!("- 平均绝对差：{:.2}\n", c.mean_abs_diff));
        md.push_str(&format!("- 偏差（judge−人工）：{:.2}\n", c.bias));
        if let Some(coef) = c.calibrated_coefficient {
            md.push_str(&format!("- 校准系数：{:.3}\n", coef));
        }
        md.push_str(&format!("- 标注：{}\n\n", c.annotation));
    }

    // 知识层质量（双口径：主口径含记忆注入 / 对照口径全部档位池化；每口径按判据分栏）
    if let Some(kq) = &report.knowledge_quality {
        md.push_str("## 知识层抽取质量评估（双口径）— 按判据口径分栏\n\n");
        md.push_str(
            "- 判据口径：legacy（旧 2-gram 覆盖，冻结）/ norm（长度归一）/ point（子句级事实点）；\n",
        );
        md.push_str("  阈值三口径共用：命中 ≥0.5 / 误报 <0.3 / 漏报 <0.4；余弦权重相同。\n\n");
        // description 已自带「主口径/对照口径」标签，标题不再重复拼接 tag。
        for scope in [&kq.primary, &kq.pooled] {
            md.push_str(&format!("### {}\n\n", scope.description));
            md.push_str("| 判据口径 | 样本 | 命中率(≥0.5) | 误报率(<0.3) | 漏报率(<0.4) | 漏报目标(<10%) |\n");
            md.push_str("|---|---|---|---|---|---|\n");
            // 旧 JSON 无判据明细 → 由 legacy 扁平字段回填单行，保证渲染路径恒有输出。
            let rows: Vec<KnowledgeJudgeRates> = if scope.judge_rates.is_empty() {
                vec![KnowledgeJudgeRates {
                    judge: "legacy".to_string(),
                    sample_count: scope.sample_count,
                    hit_rate: if scope.sample_count == 0 {
                        0.0
                    } else {
                        scope.fact_hit_count as f64 / scope.sample_count as f64
                    },
                    false_positive_rate: scope.false_positive_rate,
                    false_negative_rate: scope.false_negative_rate,
                    miss_target_met: scope.miss_target_met,
                }]
            } else {
                scope.judge_rates.clone()
            };
            for r in &rows {
                md.push_str(&format!(
                    "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {} |\n",
                    r.judge,
                    r.sample_count,
                    r.hit_rate * 100.0,
                    r.false_positive_rate * 100.0,
                    r.false_negative_rate * 100.0,
                    if r.sample_count == 0 {
                        "—"
                    } else if r.miss_target_met {
                        "达标"
                    } else {
                        "未达标"
                    }
                ));
            }
            md.push('\n');
        }
        md.push_str(&format!("- 结论：{}\n\n", kq.annotation));
    }

    // 客观风格形态指标（对照语气 judge；judge 在 20~30 字短回复上区分力不足）
    if !report.style_metrics.is_empty() {
        md.push_str("## 风格形态指标（客观口径，对照语气 judge）\n\n");
        md.push_str(
            "- 口径：只依赖回复文本，不依赖 judge。`参考重合` = 回复长度分布与 persona 参考\n",
        );
        md.push_str(
            "  （tone 题 `reference`，persona 原回复）长度分布的重叠系数（分箱 5 字、60 字封顶），\n",
        );
        md.push_str("  1.0 表示分布一致；`≤30字` 为新社交模板的目标区间占比。\n");
        md.push_str("- 用途：与语气维 judge 结论交叉验证；judge 判「无差异」时，本表可佐证差异确实不存在。\n\n");
        let ref_mean = report
            .style_metrics
            .iter()
            .find_map(|m| m.ref_len_mean)
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "—".to_string());
        md.push_str(&format!("- persona 参考均长：{ref_mean} 字\n\n"));
        md.push_str(
            "| 档位 | 均长 | 中位 | ≤30字 | 参考重合 | 语气词 | 疑问 | 感叹 | 复读 | 助手腔 |\n",
        );
        md.push_str("|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|\n");
        for m in &report.style_metrics {
            let overlap = m
                .len_ref_overlap
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "—".to_string());
            md.push_str(&format!(
                "| {} | {:.1} | {:.1} | {:.3} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                m.variant_id,
                m.len_mean,
                m.len_median,
                m.len_le_30_rate,
                overlap,
                m.tone_particle_rate,
                m.question_rate,
                m.exclaim_rate,
                m.repeat_rate,
                m.assistant_marker_rate
            ));
        }
        md.push('\n');
    }

    // 消融对比统计（按对照类型分栏：removal 移除 / substitution 替代 / increment 净增量）
    if let Some(ab) = &report.ablation {
        md.push_str("## 消融对比统计\n\n");
        md.push_str(
            "- 方法：按题目配对 Wilcoxon 符号秩检验 + Cohen's d + 95% CI；\
             多比较经 Benjamini–Hochberg FDR 校正\n",
        );
        md.push_str("- 判定线：p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0\n");
        md.push_str(&format!("- {}\n\n", ab.equivalence_note));
        md.push_str("- 对照语义（D-V20-006）：\n");
        md.push_str("  - **removal（移除，基线 F0）**：全开中逐层关闭 → 去掉某一层的边际损失；\n");
        md.push_str(
            "  - **substitution（替代，基线 B1）**：去 RAG 摘要、仅单专属层 → 单层能否替代 RAG；\n",
        );
        md.push_str("  - **increment（净增量，基线 B1）**：B1 基座 + 单专属层 → RAG 之上叠加一层的净增量。\n\n");
        md.push_str(&format!(
            "- 判定维度：{}（描述性展示：{}）\n\n",
            ab.judgment_dimensions.join(" / "),
            ab.descriptive_dimensions.join(" / ")
        ));

        let render_rows = |md: &mut String, label: &str, rows: &[&AblationComparisonRow]| {
            md.push_str(&format!("### {label}\n\n"));
            md.push_str(
                "| 消融档位 | 基线 | 维度 | 基线均分 | 档位均分 | Δ | p_fdr | d | 95%CI | 判定 |\n",
            );
            md.push_str("|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|------|\n");
            for r in rows {
                // 判定列按三态 verdict 渲染，并附 TOST p 便于核对等效结论。
                let verdict_cell = {
                    let v = match r.verdict.as_str() {
                        "significant_down" => "↓ 显著下降",
                        "significant_up" => "↑ 显著提升",
                        "equivalent" => "≡ 等效（无净增量）",
                        _ => "? 不确定",
                    };
                    format!("{v}(n={}, TOST p={:.3})", r.n_pairs, r.tost_p)
                };
                md.push_str(&format!(
                    "| {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.4} | {:.2} | [{:.3}, {:.3}] | {} |\n",
                    r.ablation_variant,
                    r.base_variant,
                    r.dimension,
                    r.base_mean,
                    r.ablated_mean,
                    r.mean_diff,
                    r.p_fdr,
                    r.cohens_d,
                    r.ci95_low,
                    r.ci95_high,
                    verdict_cell,
                ));
            }
            md.push('\n');
        };

        for (label, ctype) in [
            ("移除对照（F 组 vs F0）", "removal"),
            ("替代对照（S 组 vs B1）", "substitution"),
            ("净增量对照（I 组 vs B1）", "increment"),
        ] {
            let group: Vec<&AblationComparisonRow> = ab
                .rows
                .iter()
                .filter(|r| r.comparison_type == ctype)
                .collect();
            if !group.is_empty() {
                render_rows(&mut md, label, &group);
            }
        }

        md.push_str("### 辅助指标\n\n");
        md.push_str("| 档位 | 平均回复(字符) | 平均耗时(ms) | 空回复率 | 成功/总 |\n");
        md.push_str("|------|:---:|:---:|:---:|:---:|\n");
        for a in &ab.aux {
            md.push_str(&format!(
                "| {} | {:.1} | {:.1} | {:.1}% | {}/{} |\n",
                a.variant_id,
                a.reply_chars_mean,
                a.elapsed_ms_mean,
                a.empty_reply_rate * 100.0,
                a.success_count,
                a.total_count
            ));
        }
        md.push('\n');
    }

    // 描述性指标（口径未校准，不参与层价值判定）
    md.push_str("## 描述性指标（不参与层价值判定）\n\n");
    for note in &report.descriptive_metrics {
        md.push_str(&format!("- {note}\n"));
    }
    md.push('\n');

    // 辅助指标四件套（产物可复算近似）
    md.push_str("## 辅助指标（产物可复算）\n\n");
    let fmt_opt = |v: Option<f64>| {
        v.map(|x| format!("{:.1}%", x * 100.0))
            .unwrap_or_else(|| "-".to_string())
    };
    md.push_str(&format!(
        "- 证据链可追溯率（代理口径）：{}\n",
        fmt_opt(report.auxiliary.evidence_traceability_rate)
    ));
    md.push_str(&format!(
        "- 行为规则命中率（代理口径）：{}\n",
        fmt_opt(report.auxiliary.behavior_rule_hit_rate)
    ));
    md.push_str(&format!(
        "- 情境路由误用率（代理口径）：{}\n",
        fmt_opt(report.auxiliary.situation_route_misuse_rate)
    ));
    match report.auxiliary.profile_regression_output_stability {
        Some(s) => md.push_str(&format!(
            "- 画像回归（代理口径，跨轮 fact/tone/emotion std 均值）：{:.4}\n",
            s
        )),
        None => md.push_str(
            "- 画像回归（代理口径，跨轮 fact/tone/emotion std 均值）：-（无 --repeat 明细）\n",
        ),
    }
    md.push_str(&format!(
        "- 口径与局限：{}\n\n",
        report.auxiliary.annotation
    ));

    // 数据特性与外部效度局限（报告必出字段）
    md.push_str("## 数据特性与外部效度局限\n\n");
    if report.limitations.is_empty() {
        md.push_str("- （无附加局限说明）\n");
    } else {
        for l in &report.limitations {
            md.push_str(&format!("- {l}\n"));
        }
    }
    md.push('\n');

    md.push_str("---\n*由 `ramaria probe report` 自动生成，供 M5 定稿实验参考。*\n");
    md
}

/// 文本模式打印报告摘要（stdout 只输出数据）。
pub(crate) fn print_report_summary(report: &ProbeReport) {
    println!(
        "探针报告: persona={} | {} 档位对比 | judge={} | embedding={}",
        report.persona_uid,
        report.variants.len(),
        report.judge_used,
        report.embedding_used
    );
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_norm = r
            .fact_score_norm
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_point = r
            .fact_score_point
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  档位 {:<14} 事实={:<6} 事实归一={:<6} 事实点={:<6} 语气={:<6} 情感={:<6} 成功={}/{} — {}",
            r.variant_id,
            fact,
            fact_norm,
            fact_point,
            tone,
            emotion,
            r.success_count,
            r.total_count,
            r.description
        );
    }
    println!("定稿建议: {}", report.recommendation.overall);
    if let Some(c) = &report.calibration {
        println!(
            "校准: 样本 {}/{} 同分 {:.0}% 偏差 {:.2} {}",
            c.sample_count,
            c.total_count,
            c.consistency_exact * 100.0,
            c.bias,
            if c.inconsistent {
                "⚠ 不一致"
            } else {
                "✓ 一致"
            }
        );
    }
    if let Some(kq) = &report.knowledge_quality {
        println!(
            "知识层（主口径 含记忆注入）: 样本 {} 误报 {:.1}% 漏报 {:.1}% {} | 对照口径（全量池化）漏报 {:.1}%",
            kq.primary.sample_count,
            kq.primary.false_positive_rate * 100.0,
            kq.primary.false_negative_rate * 100.0,
            if kq.primary.miss_target_met {
                "（达标）"
            } else {
                "（未达标）"
            },
            kq.pooled.false_negative_rate * 100.0
        );
    }
    if let Some(ab) = &report.ablation {
        let sig = ab.rows.iter().filter(|r| r.significant).count();
        println!(
            "消融对比: {} 行对比（基线 {}），显著 {} 行",
            ab.rows.len(),
            ab.baseline_variant,
            sig
        );
    }
    // 辅助指标四件套摘要（产物可复算近似）
    let fmt_opt = |v: Option<f64>| {
        v.map(|x| format!("{:.1}%", x * 100.0))
            .unwrap_or_else(|| "-".to_string())
    };
    println!(
        "辅助指标: 可追溯率(代理口径)={} 规则命中(代理口径)={} 路由误用(代理口径)={} 画像回归(代理口径,std)={}",
        fmt_opt(report.auxiliary.evidence_traceability_rate),
        fmt_opt(report.auxiliary.behavior_rule_hit_rate),
        fmt_opt(report.auxiliary.situation_route_misuse_rate),
        report
            .auxiliary
            .profile_regression_output_stability
            .map(|s| format!("{:.4}", s))
            .unwrap_or_else(|| "-".to_string())
    );
    crate::ui::info("用 --output 生成 markdown/JSON 报告文件");
}
