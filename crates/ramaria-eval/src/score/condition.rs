//! crates/ramaria-eval/src/score/condition.rs - Ramaria 评测单条件判分
//!
//! 设计特点:
//! - 两遍扫描：先算确定性列（检索 / 计数 / 字面交叉 / 人格两维），再按需判分
//! - 三档互斥：失败优先、其次空 gold、其余参与判分，计数之和等于题目数
//! - 缺库即降级：解析器缺席时检索列留 null，其余列照常产出
//! - 判分按任务分四趟：分项 → 不可回答 → 冲突 → 语气，同任务连续推进不交错，趟内可并发
//! - 判分按题型裁剪：无要点不调 rubric，非 UA / CD 不调对应判定
//! - 均值只在参与判分的题上取算术平均，样本为 0 时整列留 null
//! - 用量两路分开：答案按逐题产物累加，判分按条件前后差值取增量，结果交给 usage.json

use futures::{StreamExt, stream};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, LlmUsage};

use super::artifact::{
    ArtifactRetrieval, ConditionScore, ConditionUsage, ItemRetrieval, MessageRetrieval,
    RetrievalScore, ScoredItem, UsageSummary,
};
use super::artifact_index::{self, ArtifactIndex, ArtifactMeasure};
use super::credibility::{self, JudgeTally};
use super::judge::Judge;
use super::persona::{self, PersonaSums};
use super::retrieval::{self, SourceResolver};
use super::rubric::{self, RubricSums};
use crate::run::RunItemRecord;

/// 旧产物缺判分原料时的提示文案。
const NOTE_MATERIAL_MISSING: &str = "旧产物缺分项原料";

// =========================================================
// 逐题产物
// =========================================================

/// 逐题产物与其在 `items.jsonl` 中的末次出现位置。
///
/// 字段约定:
/// - `order`: 末次出现的行号，输出时按该值升序（保持原始出现顺序）；
/// - `item`: 判分后的逐题行。
pub(super) struct ScoredEntry {
    pub order: usize,
    pub item: ScoredItem,
}

// =========================================================
// 指标累加
// =========================================================

/// 单条件消息级指标的累加器（只在参与判分的题上累加）。
#[derive(Default)]
struct MetricSums {
    items: usize,
    coverage_pre: f64,
    coverage_post: f64,
    precision_pre: f64,
    precision_post: f64,
    mrr_pre: f64,
    mrr_post: f64,
    units_pre: usize,
    units_post: usize,
    unmappable: usize,
}

impl MetricSums {
    /// 吸收一题的指标。
    fn absorb(&mut self, retrieval: &ItemRetrieval) {
        self.items += 1;
        self.coverage_pre += retrieval.coverage_pre;
        self.coverage_post += retrieval.coverage_post;
        self.precision_pre += retrieval.precision_pre;
        self.precision_post += retrieval.precision_post;
        self.mrr_pre += reciprocal_rank(retrieval.rank_pre);
        self.mrr_post += reciprocal_rank(retrieval.rank_post);
        self.units_pre += retrieval.units_pre;
        self.units_post += retrieval.units_post;
        self.unmappable += retrieval.unmappable_pre + retrieval.unmappable_post;
    }

    /// 收口为条件均值；没有参与判分的题时返回 `None`。
    fn finish(&self) -> Option<MessageRetrieval> {
        if self.items == 0 {
            return None;
        }
        let divisor = self.items as f64;
        let coverage_pre = self.coverage_pre / divisor;
        let coverage_post = self.coverage_post / divisor;
        Some(MessageRetrieval {
            questions: self.items,
            coverage_pre,
            coverage_post,
            precision_pre: self.precision_pre / divisor,
            precision_post: self.precision_post / divisor,
            mrr_pre: self.mrr_pre / divisor,
            mrr_post: self.mrr_post / divisor,
            delta_coverage: coverage_post - coverage_pre,
            units_pre_avg: self.units_pre as f64 / divisor,
            units_post_avg: self.units_post as f64 / divisor,
            unmappable_total: self.unmappable,
        })
    }
}

/// 名次倒数（未命中为 0）。
fn reciprocal_rank(rank: Option<usize>) -> f64 {
    rank.map(|rank| 1.0 / rank as f64).unwrap_or(0.0)
}

/// 单条件 artifact 级指标的累加器。
///
/// 说明: 只在拿到单题指标时累加均值，缺失 evidence 对所有可测量的题都计数。
#[derive(Default)]
struct ArtifactSums {
    items: usize,
    coverage_pre: f64,
    coverage_post: f64,
    precision_pre: f64,
    precision_post: f64,
    mrr_pre: f64,
    mrr_post: f64,
    units_pre: usize,
    units_post: usize,
    missing_gold: usize,
}

impl ArtifactSums {
    /// 吸收一题的测量结果（缺失 evidence 与均值分开处理）。
    fn absorb(&mut self, measure: &ArtifactMeasure) {
        self.missing_gold += measure.missing_gold;
        let Some(item) = &measure.item else {
            return;
        };
        self.items += 1;
        self.coverage_pre += item.coverage_pre;
        self.coverage_post += item.coverage_post;
        self.precision_pre += item.precision_pre;
        self.precision_post += item.precision_post;
        self.mrr_pre += reciprocal_rank(item.rank_pre);
        self.mrr_post += reciprocal_rank(item.rank_post);
        self.units_pre += measure.units_pre;
        self.units_post += measure.units_post;
    }

    /// 收口为条件均值；没有参与聚合的题时整列留空。
    fn finish(&self) -> Option<ArtifactRetrieval> {
        if self.items == 0 {
            return None;
        }
        let divisor = self.items as f64;
        let coverage_pre = self.coverage_pre / divisor;
        let coverage_post = self.coverage_post / divisor;
        Some(ArtifactRetrieval {
            questions: self.items,
            coverage_pre,
            coverage_post,
            precision_pre: self.precision_pre / divisor,
            precision_post: self.precision_post / divisor,
            mrr_pre: self.mrr_pre / divisor,
            mrr_post: self.mrr_post / divisor,
            delta_coverage: coverage_post - coverage_pre,
            units_pre_avg: self.units_pre as f64 / divisor,
            units_post_avg: self.units_post as f64 / divisor,
            missing_gold_total: self.missing_gold,
        })
    }
}

// =========================================================
// 条件判分
// =========================================================

/// 第一遍的确定性结果。
///
/// 字段约定:
/// - `scored` / `skipped_no_gold` / `failed_items`: 三档互斥计数；
/// - `sums` / `artifact_sums`: 两个检索口径的累加器；
/// - `entries`: 逐题行骨架（判分列留空，待第二遍填）。
struct DeterministicPass {
    scored: usize,
    skipped_no_gold: usize,
    failed_items: usize,
    sums: MetricSums,
    artifact_sums: ArtifactSums,
    entries: Vec<ScoredEntry>,
}

/// 只算确定性列：计数、检索指标与字面交叉。
///
/// 参数:
/// - `entries`: 该条件的逐题产物；
/// - `resolver`: 单元证据解析器（随本函数结束释放，判分的 await 不持有库侧借用）；
/// - `index`: 档位 artifact 反查表。
fn measure_all(
    entries: &[(usize, RunItemRecord)],
    mut resolver: Option<&mut dyn SourceResolver>,
    index: Option<&ArtifactIndex>,
) -> DeterministicPass {
    let mut pass = DeterministicPass {
        scored: 0,
        skipped_no_gold: 0,
        failed_items: 0,
        sums: MetricSums::default(),
        artifact_sums: ArtifactSums::default(),
        entries: Vec::with_capacity(entries.len()),
    };
    for (order, record) in entries {
        if record.error.is_some() {
            pass.failed_items += 1;
        } else if record.gold_message_ids.is_empty() {
            pass.skipped_no_gold += 1;
        } else {
            pass.scored += 1;
        }

        let mut retrieval = match resolver.as_deref_mut() {
            Some(source) => retrieval::measure_item(record, source),
            None => None,
        };
        if let Some(metrics) = &retrieval {
            pass.sums.absorb(metrics);
        }
        if let (Some(source), Some(index), Some(current)) =
            (resolver.as_deref_mut(), index, retrieval.as_mut())
        {
            let measure = artifact_index::measure_item(record, source, index);
            if let Some(measure) = &measure {
                pass.artifact_sums.absorb(measure);
                current.artifact = measure.item.clone();
            }
        }

        pass.entries.push(ScoredEntry {
            order: *order,
            item: ScoredItem {
                question_id: record.question_id.clone(),
                condition_id: record.condition_id.clone(),
                tier: record.tier.clone(),
                pipeline: record.pipeline.clone(),
                budget_chars: record.budget_chars,
                answer_model: record.answer_model.clone(),
                sample_key: record.sample_key.clone(),
                category: record.category.clone(),
                gold_count: record.gold_message_ids.len(),
                retrieval,
                credibility: credibility::item_credibility(record),
                rubric: None,
                persona: None,
                notes: notes_of(record),
            },
        });
    }
    pass
}

/// 判分一个条件。
///
/// 参数:
/// - `condition_id`: 条件标识；
/// - `entries`: 该条件的逐题产物（已按复合键取末行，带原始行号）；
/// - `resolver`: 单元证据解析器；`None` 表示工作库缺失或该条件无召回轨迹，检索列整列留 `null`；
/// - `index`: 档位 artifact 反查表；`None` 表示映射缺失，artifact 列留 `null`；
/// - `embedder`: 该条件工作库的嵌入 provider；`None` 表示向量通道不可用，事实维降级；
/// - `judge`: 判分句柄；`None` 表示本次不判分，判分列与语气维整列留 `null`。
///
/// 返回:
/// - 条件汇总、逐题行与本次条件的用量；题目计数满足 `questions = scored + skipped_no_gold + failed_items`；
/// - 用量属运行遥测，不进条件汇总，由调用方单独落 `usage.json`；
/// - 检索均值只统计拿到指标的题，空 gold 与失败题不计入；两列各自按自身 gold 判定参与题；
/// - 判分失败只累加 `judge_failed`，不中断条件；
/// - 判分按任务分四趟推进，趟内按句柄并发度并行、结果按题目下标装配，逐题字段与串行判分等价；
/// - 人格三维与判分句柄解耦：无 judge 时语气维留空，事实维与情感维照常产出。
pub(super) async fn score_condition(
    condition_id: &str,
    entries: Vec<(usize, RunItemRecord)>,
    resolver: Option<&mut dyn SourceResolver>,
    index: Option<&ArtifactIndex>,
    embedder: Option<&dyn EmbeddingProvider>,
    judge: Option<&Judge>,
) -> RamariaResult<(ConditionScore, Vec<ScoredEntry>, ConditionUsage)> {
    let questions = entries.len();
    // ---- 答案用量：逐题累加（失败题与未上报用量的题不计） ----
    let answer = usage_of_records(&entries);
    // ---- 第一遍：确定性列（检索指标、计数与字面交叉） ----
    let DeterministicPass {
        scored,
        skipped_no_gold,
        failed_items,
        sums,
        artifact_sums,
        entries: mut scored_entries,
    } = measure_all(&entries, resolver, index);

    // ---- 第二遍之一：人格事实维与情感维（确定性，可用嵌入） ----
    for ((_, record), entry) in entries.iter().zip(scored_entries.iter_mut()) {
        entry.item.persona = persona::score_deterministic(embedder, record).await;
    }

    // ---- 第二遍之二：判分四趟（先取用量基线，条件结束后取差值） ----
    // 同一任务的 system 提示连续复用，缓存前缀不被打断；趟内按并发度并行，跨趟仍顺序。
    let judge_before = judge_usage(judge);
    let mut rubric_sums = RubricSums::default();
    let mut judge_tally = JudgeTally::default();
    let mut judge_failed = 0usize;
    if let Some(judge) = judge {
        let concurrency = judge.concurrency();
        // 趟一：分项要点（有回复且要点非空）
        let jobs: Vec<(usize, &RunItemRecord)> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.1.reply.is_some() && !entry.1.score_points.is_empty())
            .map(|(position, entry)| (position, &entry.1))
            .collect();
        let mut results = stream::iter(jobs)
            .map(|(position, record)| async move { (position, rubric::score(judge, record).await) })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(position, _)| *position);
        for (position, result) in results {
            if let Some(rubric) = result? {
                rubric_sums.absorb(&rubric, &entries[position].1.category);
                scored_entries[position].item.rubric = Some(rubric);
            }
        }
        // 趟二：不可回答题
        let jobs: Vec<(usize, &RunItemRecord)> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .1
                    .category
                    .contains(&credibility::CATEGORY_UNANSWERABLE)
            })
            .map(|(position, entry)| (position, &entry.1))
            .collect();
        let mut results = stream::iter(jobs)
            .map(|(position, record)| async move {
                (
                    position,
                    credibility::judge_unanswerable(judge, record).await,
                )
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(position, _)| *position);
        for (position, result) in results {
            let Some(flag) = result? else { continue };
            credibility::apply_unanswerable(&mut scored_entries[position].item.credibility, &flag);
            judge_tally.absorb_unanswerable(&flag);
        }
        // 趟三：冲突题
        let jobs: Vec<(usize, &RunItemRecord)> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.1.category.contains(&credibility::CATEGORY_CONFLICT))
            .map(|(position, entry)| (position, &entry.1))
            .collect();
        let mut results = stream::iter(jobs)
            .map(|(position, record)| async move {
                (position, credibility::judge_conflict(judge, record).await)
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(position, _)| *position);
        for (position, result) in results {
            let Some(flag) = result? else { continue };
            credibility::apply_conflict(&mut scored_entries[position].item.credibility, &flag);
            judge_tally.absorb_conflict(&flag);
        }
        // 趟四：语气维（有回复且参考非空；人格骨架缺席的题不参与）
        let jobs: Vec<(usize, &RunItemRecord)> = scored_entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.item.persona.is_some())
            .map(|(position, _)| (position, &entries[position].1))
            .collect();
        let mut results = stream::iter(jobs)
            .map(|(position, record)| async move {
                (position, persona::score_tone(judge, record).await)
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(position, _)| *position);
        for (position, result) in results {
            let tone = result?;
            if let Some(persona) = scored_entries[position].item.persona.as_mut() {
                persona.tone = tone;
            }
        }
        // 判分失败只计数：任一分项或判定失败即计入，不中断条件。
        for entry in &scored_entries {
            let rubric_failed = entry
                .item
                .rubric
                .as_ref()
                .is_some_and(|rubric| rubric.judge_failed);
            if rubric_failed || entry.item.credibility.judge_failed {
                judge_failed += 1;
            }
        }
    }

    // ---- 第三遍：人格三维汇总（语气维已在趟四填入） ----
    let tone_attempted = judge.is_some();
    let mut persona_sums = PersonaSums::default();
    for entry in &scored_entries {
        if let Some(persona) = &entry.item.persona {
            persona_sums.absorb(persona, tone_attempted);
        }
    }

    let credibilities: Vec<_> = scored_entries
        .iter()
        .map(|entry| entry.item.credibility.clone())
        .collect();
    let score = ConditionScore {
        condition_id: condition_id.to_string(),
        questions,
        scored,
        skipped_no_gold,
        failed_items,
        judge_failed,
        retrieval: RetrievalScore {
            message: sums.finish(),
            artifact: artifact_sums.finish(),
        },
        credibility: credibility::aggregate(&credibilities, &judge_tally),
        rubric: rubric_sums.finish(),
        persona: persona_sums.finish(),
        missing: Vec::new(),
    };
    let usage = ConditionUsage {
        answer,
        judge: judge_delta(judge_before, judge_usage(judge)),
    };
    Ok((score, scored_entries, usage))
}

// =========================================================
// 用量
// =========================================================

/// 汇总该条件逐题产物里的答案用量。
///
/// 参数:
/// - `entries`: 该条件的逐题产物。
///
/// 返回:
/// - `calls` 为带用量的题数，其余字段为各题数值之和（无用量时为全 0）。
fn usage_of_records(entries: &[(usize, RunItemRecord)]) -> UsageSummary {
    let mut calls = 0usize;
    let mut total = LlmUsage::default();
    for (_, record) in entries {
        if let Some(usage) = record.usage {
            calls += 1;
            total.add(usage);
        }
    }
    UsageSummary::from_usage(calls, total)
}

/// 判分句柄当前用量快照（无句柄时为全 0）。
fn judge_usage(judge: Option<&Judge>) -> (LlmUsage, usize) {
    match judge {
        Some(judge) => (judge.usage(), judge.usage_calls()),
        None => (LlmUsage::default(), 0),
    }
}

/// 前后快照之差：条件级判分增量。
///
/// 说明: 判分句柄跨条件复用，只有差值才属于本条件；饱和相减保证后值不回退。
fn judge_delta(before: (LlmUsage, usize), after: (LlmUsage, usize)) -> UsageSummary {
    let delta = LlmUsage {
        prompt_tokens: after.0.prompt_tokens.saturating_sub(before.0.prompt_tokens),
        completion_tokens: after
            .0
            .completion_tokens
            .saturating_sub(before.0.completion_tokens),
        cache_hit_tokens: after
            .0
            .cache_hit_tokens
            .saturating_sub(before.0.cache_hit_tokens),
        cache_miss_tokens: after
            .0
            .cache_miss_tokens
            .saturating_sub(before.0.cache_miss_tokens),
    };
    UsageSummary::from_usage(after.1.saturating_sub(before.1), delta)
}

/// 逐题提示：旧产物缺分项判分原料时记一条，不视为错误。
fn notes_of(record: &RunItemRecord) -> Vec<String> {
    if record.score_points.is_empty() && !record.gold_message_ids.is_empty() {
        vec![NOTE_MATERIAL_MISSING.to_string()]
    } else {
        Vec::new()
    }
}
