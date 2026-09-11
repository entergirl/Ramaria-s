//! crates/ramaria-cli/src/commands/rule.rs - 行为规则管理命令
//!
//! 设计特点:
//! - 子命令遵循 §2.9 动词词表：list/show/import/edit/enable/disable/delete/evidence/relearn
//!   （`get` 仅 config 专用，规则详情用 `show`；relearn 触发 persona 全量行为学习）
//! - clusters 为只读统计：读事件 → 双通道向量化 → 原始口径密度聚类 → 输出簇结构与相似度分布，
//!   并追加 min−1 反事实对照、含 θ_nb 重试的管线口径/质控闸门估算（规则产出量），
//!   以及可选的 θ_join 时序增量模拟（留一：前段建簇 → 后段逐条喂入增量管线）
//! - 全部支持全局 `--json` 信封；stdout 只输出数据
//! - delete 为破坏性操作：交互确认 / 非 TTY 或 `--yes` 自动通过（M1 B 项）
//! - evidence 展示规则 → 事件 → 原文溯源链（只含结构化字段，原文不落日志）
//! - edit/disable 触发 H1 S1 反馈写入（行为层内部处理）

use anyhow::Context;
use ramaria_core::behavior::{BehaviorParams, BehaviorRule, RuleSource};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{MemoryEvent, now_ms};
use ramaria_memory::behavior::{
    BehaviorSample, DensityClusterResult, PendingPool, QualityVerdict, RefinedCluster,
    RuleDegradeReason, RuleGenConfig, compute_incremental_update, density_cluster,
    fused_similarity, quality_gate, refine_cluster, sample_from_event, vectorize,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::json;

// =========================================================
// 公共枚举与入口
// =========================================================

/// Rule 子命令。
#[derive(Debug, Clone)]
pub enum RuleCmd {
    /// 列出 persona 的全部规则（含禁用项）
    List {
        /// 按 persona_uid 筛选
        persona: Option<String>,
        /// 输出条数上限（None = 全部）
        limit: Option<usize>,
        /// 跳过前 N 条（分页）
        offset: usize,
    },
    /// 查看单条规则详情
    Show { id: i64 },
    /// 手工导入规则（JSON 文件，`-` = stdin）
    Import {
        /// 导入源文件路径（`-` = stdin）
        file: String,
        /// 规则所属 persona
        persona: Option<String>,
    },
    /// 编辑规则（reaction / avoid；编辑后转为 Manual 并写 S1 反馈）
    Edit {
        id: i64,
        /// 新的规则文本（缺省保留原值）
        reaction: Option<String>,
        /// 新的禁忌列表（逗号分隔，缺省保留原值）
        avoid: Option<String>,
    },
    /// 启用规则
    Enable { id: i64 },
    /// 禁用规则（写 S1 反馈）
    Disable { id: i64 },
    /// 删除规则（需确认）
    Delete { id: i64, force: bool },
    /// 展示规则证据链（规则 → 事件 → 原文摘要）
    Evidence { id: i64 },
    /// 触发 persona 全量行为学习（基于全部事件重新聚类并生成/替换 Auto 规则）
    Relearn {
        /// 规则所属 persona
        persona: Option<String>,
    },
    /// 只读统计行为聚类结构（事件 → 样本 → 向量化 → 原始密度聚类；不写库、不调 LLM）
    Clusters {
        /// 目标 persona
        persona: Option<String>,
        /// θ_nb 覆盖（0.0-1.0，缺省用配置值）
        theta_nb: Option<f64>,
        /// min_cluster_size 覆盖（≥1，缺省用配置值）
        min_cluster_size: Option<usize>,
        /// β1 覆盖（≥0，与 β2 之和 ≤ 1，缺省用配置值）
        beta1: Option<f64>,
        /// β2 覆盖（≥0，与 β1 之和 ≤ 1，缺省用配置值）
        beta2: Option<f64>,
        /// θ_join 档位列表（空 = 不启用 θ_join 时序增量模拟）
        theta_join: Vec<f64>,
        /// θ_join 时序增量模拟的前段事件占比（0.1-0.9）
        split_ratio: f64,
    },
}

/// 默认规则所属 persona（与全局默认一致）。
const DEFAULT_RULE_PERSONA: &str = "rama-0001";

/// 运行 rule 子命令分发。
///
/// 参数:
/// - `app`: App 实例引用。
/// - `cmd`: Rule 子命令。
/// - `json`: JSON 信封输出。
/// - `yes`: 自动确认所有确认点（delete 等）。
pub async fn run(
    app: &Arc<ramaria_app::App>,
    cmd: RuleCmd,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    match cmd {
        RuleCmd::List {
            persona,
            limit,
            offset,
        } => run_list(app, persona, limit, offset, json).await,
        RuleCmd::Show { id } => run_show(app, id, json).await,
        RuleCmd::Import { file, persona } => run_import(app, &file, persona, json).await,
        RuleCmd::Edit {
            id,
            reaction,
            avoid,
        } => run_edit(app, id, reaction, avoid, json).await,
        RuleCmd::Enable { id } => run_set_enabled(app, id, true, json).await,
        RuleCmd::Disable { id } => run_set_enabled(app, id, false, json).await,
        RuleCmd::Delete { id, force } => run_delete(app, id, force, json, yes).await,
        RuleCmd::Evidence { id } => run_evidence(app, id, json).await,
        RuleCmd::Relearn { persona } => run_relearn(app, persona, json).await,
        RuleCmd::Clusters {
            persona,
            theta_nb,
            min_cluster_size,
            beta1,
            beta2,
            theta_join,
            split_ratio,
        } => {
            run_clusters(
                app,
                persona,
                theta_nb,
                min_cluster_size,
                beta1,
                beta2,
                theta_join,
                split_ratio,
                json,
            )
            .await
        }
    }
}

// =========================================================
// list
// =========================================================

/// 列出 persona 的行为规则。
async fn run_list(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    limit: Option<usize>,
    offset: usize,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());
    let mut rules = ramaria_app::commands::behavior::behavior_list_rules(app, &persona_uid)
        .await
        .context("查询行为规则失败")?;

    // 分页（列表命令统一 --limit/--offset 约定）
    let total = rules.len();
    if let Some(limit) = limit {
        rules = rules.into_iter().skip(offset).take(limit).collect();
    } else if offset > 0 {
        rules = rules.into_iter().skip(offset).collect();
    }

    if json {
        // 结构化输出：规则列表 + 分页前总数（与 L1/L2/L3 一致）
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "total": total,
            "rules": rules,
        });
        return json::emit_ok(&data);
    }

    if rules.is_empty() {
        crate::ui::info(&format!("人格 {persona_uid} 暂无行为规则"));
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled("规则数", &format!("{total}"));
    crate::ui::separator();
    for rule in &rules {
        let status = if rule.enabled { "启用" } else { "禁用" };
        let source = if rule.source == ramaria_core::behavior::RuleSource::Manual {
            "Manual"
        } else {
            "Auto"
        };
        let reaction = rule
            .reaction
            .as_deref()
            .unwrap_or("（候选规则，仅参数注入）");
        println!("#{} [{}] [{}] {reaction}", rule.id, source, status);
        println!("   情境: {}", rule.situation.keywords.join("、"));
        println!(
            "   置信度 {:.2} · 稳定性 {:.2} · 证据 {} 条",
            rule.confidence,
            rule.stability,
            rule.evidence.len()
        );
    }
    Ok(())
}

// =========================================================
// show
// =========================================================

/// 查看单条规则详情。
async fn run_show(app: &Arc<ramaria_app::App>, id: i64, json: bool) -> anyhow::Result<()> {
    let rule = ramaria_app::commands::behavior::behavior_get_rule(app, id)
        .await
        .context("查询行为规则失败")?
        .ok_or_else(|| anyhow::anyhow!("行为规则 {id} 不存在"))?;

    if json {
        return json::emit_ok(&rule);
    }

    crate::ui::separator();
    crate::ui::labeled("ID", &rule.id.to_string());
    crate::ui::labeled("Persona", &rule.persona_uid);
    crate::ui::labeled(
        "来源",
        if rule.source == ramaria_core::behavior::RuleSource::Manual {
            "Manual（人工）"
        } else {
            "Auto（自动学习）"
        },
    );
    crate::ui::labeled("状态", if rule.enabled { "启用" } else { "禁用" });
    crate::ui::labeled(
        "规则文本",
        rule.reaction
            .as_deref()
            .unwrap_or("（候选规则，仅参数注入）"),
    );
    crate::ui::labeled("情境关键词", &rule.situation.keywords.join("、"));
    crate::ui::labeled(
        "情感强度",
        &format!("{:.2}", rule.params.emotional_intensity),
    );
    crate::ui::labeled("主动程度", &format!("{:.2}", rule.params.proactiveness));
    crate::ui::labeled("详细度", &format!("{:.2}", rule.params.detail_level));
    crate::ui::labeled("正式度", &format!("{:.2}", rule.params.formality));
    let avoid_display = if rule.avoid.is_empty() {
        "（无）".to_string()
    } else {
        rule.avoid.join("、")
    };
    crate::ui::labeled("禁忌列表", &avoid_display);
    crate::ui::labeled("置信度", &format!("{:.2}", rule.confidence));
    crate::ui::labeled("稳定性", &format!("{:.2}", rule.stability));
    crate::ui::labeled("证据数", &rule.evidence.len().to_string());
    crate::ui::separator();
    Ok(())
}

// =========================================================
// import
// =========================================================

/// 手工导入规则（JSON 校验在行为层执行）。
async fn run_import(
    app: &Arc<ramaria_app::App>,
    file: &str,
    persona: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());
    // `-` = stdin；否则读文件
    let content = if file == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("读取 stdin 失败")?;
        buf
    } else {
        std::fs::read_to_string(file).with_context(|| format!("读取文件 {file} 失败"))?
    };

    let id = ramaria_app::commands::behavior::behavior_import_rule(app, &persona_uid, &content)
        .await
        .context("规则导入校验失败")?;

    if json {
        let data = serde_json::json!({
            "id": id,
            "persona_uid": persona_uid,
            "source": "manual",
        });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 导入成功（Manual，自动生效）"));
    Ok(())
}

// =========================================================
// edit / enable / disable
// =========================================================

/// 编辑规则（reaction / avoid；编辑后转为 Manual 并写 S1 反馈）。
async fn run_edit(
    app: &Arc<ramaria_app::App>,
    id: i64,
    reaction: Option<String>,
    avoid: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    if reaction.is_none() && avoid.is_none() {
        anyhow::bail!("请至少提供 --reaction 或 --avoid 之一");
    }
    let mut rule = ramaria_app::commands::behavior::behavior_get_rule(app, id)
        .await
        .context("查询行为规则失败")?
        .ok_or_else(|| anyhow::anyhow!("行为规则 {id} 不存在"))?;

    if let Some(r) = reaction {
        let trimmed = r.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("--reaction 不能为空（候选规则请用 import 导入 params）");
        }
        rule.reaction = Some(trimmed);
    }
    if let Some(a) = avoid {
        rule.avoid = a
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    ramaria_app::commands::behavior::behavior_edit_rule(app, &mut rule, None)
        .await
        .context("编辑行为规则失败")?;

    if json {
        let data = serde_json::json!({
            "id": id,
            "source": "manual",
            "reaction": rule.reaction,
            "avoid": rule.avoid,
        });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 已编辑（转为 Manual，S1 反馈已记录）"));
    Ok(())
}

/// 启用/禁用规则（disable 写 S1 反馈）。
async fn run_set_enabled(
    app: &Arc<ramaria_app::App>,
    id: i64,
    enabled: bool,
    json: bool,
) -> anyhow::Result<()> {
    ramaria_app::commands::behavior::behavior_set_rule_enabled(app, id, enabled, None)
        .await
        .context("切换规则状态失败")?;

    let action = if enabled { "启用" } else { "禁用" };
    if json {
        let data = serde_json::json!({ "id": id, "enabled": enabled });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 已{action}"));
    Ok(())
}

// =========================================================
// delete
// =========================================================

/// 删除规则（破坏性操作：确认 / --yes / --force）。
async fn run_delete(
    app: &Arc<ramaria_app::App>,
    id: i64,
    force: bool,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    // 先确认规则存在（避免删除不存在的 id 静默成功）
    ramaria_app::commands::behavior::behavior_get_rule(app, id)
        .await
        .context("查询行为规则失败")?
        .ok_or_else(|| anyhow::anyhow!("行为规则 {id} 不存在"))?;

    let confirmed = force
        || crate::ui::confirm(&format!("确定删除行为规则 #{id} 吗？"), yes)
            .map_err(|e| anyhow::anyhow!(e))?;
    if !confirmed {
        if json {
            // 用户主动取消：非错误（ok:true + cancelled 标志，exit 0）
            let data = serde_json::json!({ "id": id, "cancelled": true });
            return json::emit_ok(&data);
        }
        crate::ui::info(&format!("删除规则 #{id} 已取消"));
        return Ok(());
    }

    ramaria_app::commands::behavior::behavior_delete_rule(app, id)
        .await
        .context("删除行为规则失败")?;

    if json {
        let data = serde_json::json!({ "id": id, "deleted": true });
        return json::emit_ok(&data);
    }
    crate::ui::success(&format!("规则 #{id} 已删除"));
    Ok(())
}

// =========================================================
// evidence
// =========================================================

/// 展示规则证据链（规则 → 事件 → 原文摘要，只含结构化字段）。
async fn run_evidence(app: &Arc<ramaria_app::App>, id: i64, json: bool) -> anyhow::Result<()> {
    let items = ramaria_app::commands::behavior::behavior_rule_evidence(app, id)
        .await
        .context("查询规则证据失败")?;

    if json {
        let data = serde_json::json!({
            "rule_id": id,
            "evidence": items,
        });
        return json::emit_ok(&data);
    }

    if items.is_empty() {
        crate::ui::info(&format!("规则 #{id} 暂无证据（手工导入规则通常无证据链）"));
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("规则 ID", &id.to_string());
    crate::ui::labeled("证据条数", &items.len().to_string());
    crate::ui::separator();
    for (i, item) in items.iter().enumerate() {
        println!(
            "[{}] 事件 #{} (权重 {:.2})",
            i + 1,
            item.event_id,
            item.weight
        );
        println!("    标题: {}", item.title);
        println!("    摘要: {}", item.summary);
        if let Some(p) = &item.paraphrase {
            println!("    态度（脱敏）: {p}");
        }
        if let Some(kw) = &item.keywords {
            println!("    关键词: {kw}");
        }
    }
    crate::ui::separator();
    Ok(())
}

// =========================================================
// relearn
// =========================================================

/// 触发 persona 全量行为学习（事件 → 聚类 → 规则生成 → 替换旧 Auto 规则）。
///
/// 说明:
/// - 无事件时返回空统计（不报错），用于手动补跑规则学习的幂等入口。
/// - 调用 app 层 `behavior_learn`；行为层配置关闭时同样返回空统计。
async fn run_relearn(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());
    let outcome = ramaria_app::commands::behavior::behavior_learn(app, &persona_uid)
        .await
        .context("行为学习失败")?;

    if json {
        // BehaviorLearnOutcome 无 Serialize，按字段构造对象
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "event_count": outcome.event_count,
            "cluster_count": outcome.cluster_count,
            "full_rule_count": outcome.full_rule_count,
            "candidate_rule_count": outcome.candidate_rule_count,
            "replaced_rule_count": outcome.replaced_rule_count,
        });
        return json::emit_ok(&data);
    }

    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled("输入事件数", &outcome.event_count.to_string());
    crate::ui::labeled("生成簇数", &outcome.cluster_count.to_string());
    crate::ui::labeled("完整规则数", &outcome.full_rule_count.to_string());
    crate::ui::labeled("候选规则数", &outcome.candidate_rule_count.to_string());
    crate::ui::labeled(
        "被替换旧 Auto 规则数",
        &outcome.replaced_rule_count.to_string(),
    );
    crate::ui::separator();
    crate::ui::success(&format!("人格 {persona_uid} 行为学习完成"));
    Ok(())
}

// =========================================================
// clusters（只读统计）
// =========================================================

/// 相似度全对计算的对数上限（超过则跳过分布统计，避免长耗时）。
const MAX_SIMILARITY_PAIRS: usize = 500_000;

/// 只读统计 persona 的行为聚类结构。
///
/// 用法:
/// - `ramaria rule clusters [--persona <uid>] [--theta-nb <f64>] [--min-cluster-size <n>] [--beta1 <f64>] [--beta2 <f64>] [--theta-join <f64>...] [--split-ratio <f64>]`
///
/// 参数:
/// - `app`: App 实例引用。
/// - `persona`: 目标 persona（None = 默认 persona）。
/// - `theta_nb` / `min_cluster_size` / `beta1` / `beta2`: 本次计算的参数覆盖（None = 取行为配置值）。
/// - `theta_join`: θ_join 档位列表（空 = 不启用时序增量模拟）。
/// - `split_ratio`: 增量模拟的前段事件占比。
/// - `json`: JSON 信封输出。
///
/// 说明:
/// - 只读：不写库、不调用 LLM；embedding 不可用时降级纯关键词通道并在输出中标注。
/// - 参数覆盖仅作用于本次计算，不回写配置。
/// - 聚类走原始口径（不经孤立点比例超限的 θ_nb 重试），`retry_would_fire` 标记重试是否会触发。
/// - 追加 `counterfactual`：min_cluster_size − 1 的反事实对照，量化核心口径对成簇的影响。
/// - 追加 `pipeline`：复刻真实管线的 θ_nb 重试口径并逐簇过质控闸门，估算规则产出量。
/// - 追加 `incremental`（可选）：θ_join 时序增量模拟（前段建簇为模板规则，后段逐条喂入增量管线，
///   输出各档位归簇率/待定/新簇/一致性对照）；未传 `--theta-join` 时为 null。
// 参数为只读统计命令的完整输入集合（含输出模式与模拟档位透传），逐一显式传递保持可读性；
// 与 probe run 的 `run_experiment` 采用同一 allow 约定。
#[allow(clippy::too_many_arguments)]
async fn run_clusters(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    theta_nb: Option<f64>,
    min_cluster_size: Option<usize>,
    beta1: Option<f64>,
    beta2: Option<f64>,
    theta_join: Vec<f64>,
    split_ratio: f64,
    json: bool,
) -> anyhow::Result<()> {
    let persona_uid = persona.unwrap_or_else(|| DEFAULT_RULE_PERSONA.to_string());

    // 本次计算参数：行为配置克隆 + CLI 覆盖（仅本次计算，不回写配置）；
    // 克隆体同时用于管线口径复刻与质控闸门阈值派生（RuleGenConfig::from）
    let mut behavior = app.config().behavior.clone();
    if let Some(v) = theta_nb {
        behavior.theta_nb = v;
    }
    if let Some(v) = min_cluster_size {
        behavior.min_cluster_size = v;
    }
    if let Some(v) = beta1 {
        behavior.beta1 = v;
    }
    if let Some(v) = beta2 {
        behavior.beta2 = v;
    }
    let theta_nb = behavior.theta_nb;
    let min_cluster_size = behavior.min_cluster_size;
    let beta1 = behavior.beta1;
    let beta2 = behavior.beta2;
    validate_cluster_params(theta_nb, min_cluster_size, beta1, beta2)?;
    validate_incremental_params(&theta_join, split_ratio)?;
    let beta3 = (1.0 - beta1 - beta2).max(0.0);

    // 事件（只读查询）
    let events = app
        .storage()
        .list_events_by_persona(&persona_uid, 0, i64::MAX)
        .await
        .context("查询行为事件失败")?;
    if events.is_empty() && !json {
        crate::ui::info(&format!(
            "人格 {persona_uid} 暂无行为事件，跳过行为聚类统计"
        ));
        return Ok(());
    }

    // 样本与双通道向量化（embedding 不可用 → 纯关键词降级，不阻塞）
    let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
    let embedder = app.embedding_provider();
    let embedding_available = embedder.is_some();
    vectorize(&mut samples, &events, embedder.as_deref())
        .await
        .context("行为样本向量化失败")?;

    // 原始口径密度聚类（不走 BehaviorClusterer 的 θ_nb 重试路径）
    let cluster_result = density_cluster(&samples, theta_nb, min_cluster_size, beta1, beta2);
    let mut sizes: Vec<usize> = cluster_result
        .clusters
        .iter()
        .map(|c| c.member_indices.len())
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let shape = summarize_cluster_shapes(&sizes, samples.len());
    let retry_would_fire = shape.outlier_ratio > behavior.max_outlier_ratio;

    // 相似度分布（全对融合相似度；对数超限 → 跳过并记 warn）
    let similarity = compute_similarity_stats(&samples, beta1, beta2);

    // 事件级覆盖统计（向量覆盖取自向量化后的样本）
    let paraphrase_non_empty = events
        .iter()
        .filter(|e| has_text(e.paraphrase.as_deref()))
        .count();
    let attitude_non_empty = events
        .iter()
        .filter(|e| has_text(e.attitude.as_deref()))
        .count();
    let keywords_non_empty = events
        .iter()
        .filter(|e| has_text(e.keywords.as_deref()))
        .count();
    let reaction_vector_non_empty = samples
        .iter()
        .filter(|s| s.reaction_vector.is_some())
        .count();
    let situation_vector_non_empty = samples
        .iter()
        .filter(|s| s.situation_vector.is_some())
        .count();

    // ---- min−1 反事实对照（评估 min_cluster_size 口径影响；θ/β 与本次计算一致） ----
    let counterfactual: Option<CounterfactualSummary> = if min_cluster_size <= 1 {
        // 不存在更小有效口径（下限 1）：跳过反事实
        None
    } else {
        let cf_min = min_cluster_size - 1;
        let cf_result = density_cluster(&samples, theta_nb, cf_min, beta1, beta2);
        let cf_sizes: Vec<usize> = cf_result
            .clusters
            .iter()
            .map(|c| c.member_indices.len())
            .collect();
        Some(summarize_counterfactual(
            cf_min,
            &cf_sizes,
            samples.len(),
            shape.outlier_count,
        ))
    };
    let counterfactual_skip_reason: Option<String> = if counterfactual.is_none() {
        Some(format!(
            "min_cluster_size ≤ 1（当前 {min_cluster_size}），不存在更小有效口径（下限 1），跳过反事实对照"
        ))
    } else {
        None
    };

    // ---- 真实管线口径（复刻 θ_nb 重试）+ 质控闸门产出量估算 ----
    let gate_config = RuleGenConfig::from(&behavior);
    let pipeline = estimate_pipeline(&samples, &behavior, &gate_config);

    // ---- θ_join 时序增量模拟（仅 --theta-join 时；只读、不调 LLM） ----
    let incremental: Option<IncrementalSimulation> = if theta_join.is_empty() {
        None
    } else {
        Some(
            run_incremental_simulation(
                &persona_uid,
                &events,
                &samples,
                &behavior,
                embedder.as_deref(),
                &theta_join,
                split_ratio,
            )
            .await
            .context("θ_join 时序增量模拟失败")?,
        )
    };
    let incremental_json = match &incremental {
        None => serde_json::Value::Null,
        Some(IncrementalSimulation::Skipped { reason }) => serde_json::json!({
            "skipped": true,
            "skip_reason": reason,
        }),
        Some(IncrementalSimulation::Done(report)) => serde_json::json!({
            "split_ratio": report.split_ratio,
            "first_count": report.first_count,
            "second_count": report.second_count,
            "by_theta_join": report
                .by_theta_join
                .iter()
                .map(|row| serde_json::json!({
                    "theta_join": row.theta_join,
                    "assigned": row.assigned,
                    "assigned_rate": row.assigned_rate,
                    "pending_remaining": row.pending_remaining,
                    "new_clusters": row.new_clusters,
                    "new_cluster_sizes": row.new_cluster_sizes,
                    "low_confidence": row.low_confidence,
                    "decayed_rules": row.decayed_rules,
                    "drift_triggered": row.drift_triggered,
                    "agreement_checked": row.agreement_checked,
                    "agreement_rate": row.agreement_rate,
                }))
                .collect::<Vec<_>>(),
        }),
    };

    if json {
        let counterfactual_json = match &counterfactual {
            Some(cf) => serde_json::json!({
                "min_cluster_size": cf.min_cluster_size,
                "count": cf.count,
                "sizes": cf.sizes,
                "outlier_count": cf.outlier_count,
                "outlier_ratio": cf.outlier_ratio,
                "recovered_samples": cf.recovered_samples,
                "three_member_clusters": cf.three_member_clusters,
            }),
            None => serde_json::Value::Null,
        };
        let similarity_json = match &similarity {
            SimilarityDistribution::Computed(stats) => serde_json::json!({
                "pairs": stats.pairs,
                "min": stats.min,
                "p25": stats.p25,
                "p50": stats.p50,
                "p75": stats.p75,
                "p90": stats.p90,
                "max": stats.max,
                "mean": stats.mean,
                "skipped": false,
                "skip_reason": null,
            }),
            SimilarityDistribution::Skipped(reason) => serde_json::json!({
                "pairs": null,
                "min": null,
                "p25": null,
                "p50": null,
                "p75": null,
                "p90": null,
                "max": null,
                "mean": null,
                "skipped": true,
                "skip_reason": reason,
            }),
        };
        let data = serde_json::json!({
            "persona_uid": persona_uid,
            "embedding_available": embedding_available,
            "embedding_note": if embedding_available {
                serde_json::Value::Null
            } else {
                serde_json::json!("embedding 不可用，纯关键词降级")
            },
            "params": {
                "theta_nb": theta_nb,
                "min_cluster_size": min_cluster_size,
                "beta1": beta1,
                "beta2": beta2,
                "beta3": beta3,
                "max_outlier_ratio": behavior.max_outlier_ratio,
                "retry_would_fire": retry_would_fire,
            },
            "events": {
                "total": events.len(),
                "paraphrase_non_empty": paraphrase_non_empty,
                "attitude_non_empty": attitude_non_empty,
                "keywords_non_empty": keywords_non_empty,
                "reaction_vector_non_empty": reaction_vector_non_empty,
                "situation_vector_non_empty": situation_vector_non_empty,
            },
            "similarity": similarity_json,
            "clusters": {
                "count": cluster_result.cluster_count,
                "sizes": sizes,
                "max_share": shape.max_share,
                "outlier_count": shape.outlier_count,
                "outlier_ratio": shape.outlier_ratio,
                "coverage": 1.0 - shape.outlier_ratio,
            },
            "counterfactual": counterfactual_json,
            "counterfactual_skip_reason": counterfactual_skip_reason,
            "pipeline": {
                "retries_used": pipeline.retries_used,
                "effective_theta_nb": pipeline.effective_theta_nb,
                "count": pipeline.cluster_count,
                "sizes": pipeline.sizes,
                "outlier_ratio": pipeline.outlier_ratio,
                "gate": {
                    "pass": pipeline.gate.pass,
                    "low_evidence": pipeline.gate.low_evidence,
                    "low_neff": pipeline.gate.low_neff,
                    "high_valence_variance": pipeline.gate.high_valence_variance,
                },
            },
            "incremental": incremental_json,
        });
        return json::emit_ok(&data);
    }

    // 人读输出：参数 → 事件覆盖 → 相似度分布 → 簇结构
    crate::ui::separator();
    crate::ui::labeled("Persona", &persona_uid);
    crate::ui::labeled(
        "向量通道",
        if embedding_available {
            "双通道（embedding 可用）"
        } else {
            "纯关键词降级（embedding 不可用）"
        },
    );
    crate::ui::labeled("事件数", &events.len().to_string());
    crate::ui::labeled("θ_nb", &format!("{theta_nb:.3}"));
    crate::ui::labeled("min_cluster_size", &min_cluster_size.to_string());
    crate::ui::labeled(
        "β1 / β2 / β3",
        &format!("{beta1:.3} / {beta2:.3} / {beta3:.3}"),
    );
    crate::ui::labeled(
        "max_outlier_ratio",
        &format!("{:.3}", behavior.max_outlier_ratio),
    );
    crate::ui::labeled("重试会触发", if retry_would_fire { "是" } else { "否" });
    crate::ui::labeled(
        "字段覆盖",
        &format!(
            "paraphrase {paraphrase_non_empty} · attitude {attitude_non_empty} · keywords {keywords_non_empty}"
        ),
    );
    crate::ui::labeled(
        "向量覆盖",
        &format!("反应通道 {reaction_vector_non_empty} · 情境通道 {situation_vector_non_empty}"),
    );
    match &similarity {
        SimilarityDistribution::Computed(stats) => crate::ui::labeled(
            "相似度分布",
            &format!(
                "pairs {} · min {:.3} · P25 {:.3} · P50 {:.3} · P75 {:.3} · P90 {:.3} · max {:.3} · mean {:.3}",
                stats.pairs,
                stats.min,
                stats.p25,
                stats.p50,
                stats.p75,
                stats.p90,
                stats.max,
                stats.mean
            ),
        ),
        SimilarityDistribution::Skipped(reason) => {
            crate::ui::labeled("相似度分布", &format!("（跳过）{reason}"))
        }
    }
    crate::ui::labeled("簇数", &cluster_result.cluster_count.to_string());
    crate::ui::labeled("簇规模（降序）", &format!("{sizes:?}"));
    crate::ui::labeled("最大簇占比", &format!("{:.3}", shape.max_share));
    crate::ui::labeled(
        "孤立点",
        &format!(
            "{}（{:.1}%）",
            shape.outlier_count,
            shape.outlier_ratio * 100.0
        ),
    );
    crate::ui::labeled("覆盖率", &format!("{:.3}", 1.0 - shape.outlier_ratio));

    // 反事实对照（两行）
    match &counterfactual {
        Some(cf) => {
            crate::ui::labeled(
                "反事实对照",
                &format!(
                    "min_cluster_size {} · 簇数 {} · 孤立点 {}（{:.1}%）",
                    cf.min_cluster_size,
                    cf.count,
                    cf.outlier_count,
                    cf.outlier_ratio * 100.0
                ),
            );
            crate::ui::labeled(
                "反事实细节",
                &format!(
                    "3 成员簇数 {} · 回收样本数 {}",
                    cf.three_member_clusters, cf.recovered_samples
                ),
            );
        }
        None => {
            let reason = counterfactual_skip_reason
                .as_deref()
                .unwrap_or("不存在更小有效口径");
            crate::ui::labeled("反事实对照", &format!("（跳过）{reason}"));
        }
    }

    // 真实管线口径小节（含 θ_nb 重试）与质控闸门
    crate::ui::separator();
    crate::ui::labeled("管线口径", "含 θ_nb 重试（真实管线复刻）");
    crate::ui::labeled("重试次数", &pipeline.retries_used.to_string());
    crate::ui::labeled("实际 θ_nb", &format!("{:.3}", pipeline.effective_theta_nb));
    crate::ui::labeled("管线簇数", &pipeline.cluster_count.to_string());
    crate::ui::labeled("管线簇规模（降序）", &format!("{:?}", pipeline.sizes));
    crate::ui::labeled("管线孤立点比例", &format!("{:.3}", pipeline.outlier_ratio));
    crate::ui::labeled(
        "质控闸门",
        &format!(
            "通过 {} · 证据不足 {} · n_eff 不足 {} · valence 方差超限 {}",
            pipeline.gate.pass,
            pipeline.gate.low_evidence,
            pipeline.gate.low_neff,
            pipeline.gate.high_valence_variance
        ),
    );
    crate::ui::labeled(
        "闸门阈值",
        &format!(
            "证据 ≥ {} · n_eff ≥ {} · valence σ ≤ {:.3}",
            gate_config.min_evidence, gate_config.min_n_eff, gate_config.valence_std_limit
        ),
    );

    // θ_join 时序增量模拟小节（仅 --theta-join 时输出）
    match &incremental {
        None => {}
        Some(IncrementalSimulation::Skipped { reason }) => {
            crate::ui::labeled("θ_join 增量模拟", &format!("（跳过）{reason}"));
        }
        Some(IncrementalSimulation::Done(report)) => {
            crate::ui::separator();
            crate::ui::labeled(
                "θ_join 增量模拟",
                &format!(
                    "split_ratio {:.2} · 前段 {} 条 / 后段 {} 条（留一：前段建簇 → 后段逐条喂入）",
                    report.split_ratio, report.first_count, report.second_count
                ),
            );
            println!(
                "  {:<8} {:<10} {:<10} {:<8} {:<10} {:<6}",
                "θ_join", "归簇率", "待定剩余", "新簇", "一致率", "漂移"
            );
            for row in &report.by_theta_join {
                let agreement = match row.agreement_rate {
                    Some(rate) => format!("{rate:.3}"),
                    None => "—".to_string(),
                };
                println!(
                    "  {:<8.3} {:<10.3} {:<10} {:<8} {:<10} {:<6}",
                    row.theta_join,
                    row.assigned_rate,
                    row.pending_remaining,
                    row.new_clusters,
                    agreement,
                    if row.drift_triggered { "是" } else { "否" },
                );
            }
        }
    }
    crate::ui::separator();
    Ok(())
}

// =========================================================
// clusters 辅助：反事实对照与管线口径估算
// =========================================================

/// min−1 反事实聚类汇总。
///
/// 职责:
/// - 量化 `min_cluster_size` 口径对成簇的影响：口径放宽 1 后回收的样本与小簇数量。
///
/// 字段约定:
/// - `min_cluster_size`: 反事实口径（本次覆盖值 − 1，下限 1）。
/// - `count` / `sizes`（降序）/ `outlier_count` / `outlier_ratio`: 反事实聚类结构。
/// - `recovered_samples`: 原始孤立数 − 反事实孤立数（> 0 = 口径放宽后回收的样本数）。
/// - `three_member_clusters`: 反事实中各簇规模恰为 3 的簇数量。本次 `min_cluster_size = 3`
///   （反事实 = 2）时即"被 min=3 口径整体孤置的 3 样本小簇"数量；其余 min 口径下该字段
///   固定按"规模恰为 3"计数，不再对应"被原口径孤置的增量小簇"（其规模为本次 min），
///   仅作规模参考。
#[derive(Debug, Clone, PartialEq)]
struct CounterfactualSummary {
    min_cluster_size: usize,
    count: usize,
    sizes: Vec<usize>,
    outlier_count: usize,
    outlier_ratio: f64,
    recovered_samples: i64,
    three_member_clusters: usize,
}

/// 汇总 min−1 反事实聚类结果（纯函数）。
///
/// 参数:
/// - `min_cluster_size`: 反事实口径。
/// - `cluster_sizes`: 反事实各簇成员数（顺序无关，内部按降序排序）。
/// - `total`: 参与聚类的样本总数（含孤立点）。
/// - `original_outlier_count`: 原始口径的孤立点数（`recovered_samples` 的基准）。
///
/// 返回:
/// - 反事实簇结构统计；`total = 0` 时孤立点统计为 0。
fn summarize_counterfactual(
    min_cluster_size: usize,
    cluster_sizes: &[usize],
    total: usize,
    original_outlier_count: usize,
) -> CounterfactualSummary {
    let mut sizes = cluster_sizes.to_vec();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let shape = summarize_cluster_shapes(&sizes, total);
    CounterfactualSummary {
        min_cluster_size,
        count: sizes.len(),
        three_member_clusters: sizes.iter().filter(|&&s| s == 3).count(),
        recovered_samples: original_outlier_count as i64 - shape.outlier_count as i64,
        sizes,
        outlier_count: shape.outlier_count,
        outlier_ratio: shape.outlier_ratio,
    }
}

/// 质控闸门归类计数。
///
/// 字段约定:
/// - `pass`: 通过闸门的簇数（可生成完整规则）。
/// - `low_evidence` / `low_neff` / `high_valence_variance`: 各降级原因的簇数；
///   三类互斥（`quality_gate` 按证据量 → n_eff → valence 方差的顺序返回首个不满足项）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GateTally {
    pass: usize,
    low_evidence: usize,
    low_neff: usize,
    high_valence_variance: usize,
}

/// 逐簇判定质控闸门并归类计数（纯函数）。
///
/// 参数:
/// - `clusters`: 提炼后的簇列表（`refine_cluster` 输出）。
/// - `config`: 闸门阈值（由 `RuleGenConfig::from(&BehaviorConfig)` 派生）。
///
/// 返回:
/// - 通过数与三类降级原因计数。
fn tally_quality_gate(clusters: &[RefinedCluster], config: &RuleGenConfig) -> GateTally {
    let mut tally = GateTally::default();
    for cluster in clusters {
        match quality_gate(cluster, config) {
            QualityVerdict::Pass => tally.pass += 1,
            QualityVerdict::Degrade(RuleDegradeReason::LowEvidence) => tally.low_evidence += 1,
            QualityVerdict::Degrade(RuleDegradeReason::LowNeff) => tally.low_neff += 1,
            QualityVerdict::Degrade(RuleDegradeReason::HighValenceVariance) => {
                tally.high_valence_variance += 1;
            }
            // `quality_gate` 只产生以上三类原因；其余变体来自 LLM 翻译阶段，
            // 不在闸门口径内，防御性忽略并记 warn。
            QualityVerdict::Degrade(other) => {
                tracing::warn!(reason = ?other, "质控闸门返回预期外的降级原因，未计入统计");
            }
        }
    }
    tally
}

/// 带 θ_nb 重试的密度聚类执行结果。
///
/// 字段约定:
/// - `result`: 最终一次聚类的完整结果（含簇与孤立点比例）。
/// - `retries_used`: 实际发生的 θ_nb 下调重试次数（0..=2）。
/// - `effective_theta_nb`: 最终实际使用的 θ_nb。
struct ClusterRunOutcome {
    result: DensityClusterResult,
    retries_used: usize,
    effective_theta_nb: f64,
}

/// 执行真实管线口径的密度聚类（含 θ_nb 重试；纯计算）。
///
/// 说明:
/// - 复刻 `BehaviorClusterer::cluster_samples` 的重试规则（本命令不复用其入口，
///   以保证统计过程可观测）：先按 θ_nb 聚类；孤立点比例 > `max_outlier_ratio`
///   时每次 θ_nb − 0.1（下限 0.05）重试，至多 2 次。
/// - `pipeline` 估算与 θ_join 模拟的前段/全量建簇共用本口径，保证对照一致。
///
/// 参数:
/// - `samples`: 已向量化的行为样本。
/// - `behavior`: 本次计算的行为配置（θ_nb/min_cluster_size/β 权重/孤立点比例上限）。
fn cluster_with_retry(samples: &[BehaviorSample], behavior: &BehaviorConfig) -> ClusterRunOutcome {
    let mut theta_nb = behavior.theta_nb;
    let mut result = density_cluster(
        samples,
        theta_nb,
        behavior.min_cluster_size,
        behavior.beta1,
        behavior.beta2,
    );
    let mut retries_used = 0usize;
    while result.outlier_ratio > behavior.max_outlier_ratio && retries_used < 2 {
        theta_nb = (theta_nb - 0.1).max(0.05);
        result = density_cluster(
            samples,
            theta_nb,
            behavior.min_cluster_size,
            behavior.beta1,
            behavior.beta2,
        );
        retries_used += 1;
    }
    ClusterRunOutcome {
        result,
        retries_used,
        effective_theta_nb: theta_nb,
    }
}

/// 真实管线口径（含 θ_nb 重试）的规则产出量估算。
///
/// 字段约定:
/// - `retries_used`: 实际发生的 θ_nb 下调重试次数（0..=2）。
/// - `effective_theta_nb`: 最终实际使用的 θ_nb。
/// - `cluster_count` / `sizes`（降序）/ `outlier_ratio`: 最终聚类结构。
/// - `gate`: 逐簇过质控闸门的归类计数。
struct PipelineEstimate {
    retries_used: usize,
    effective_theta_nb: f64,
    cluster_count: usize,
    sizes: Vec<usize>,
    outlier_ratio: f64,
    gate: GateTally,
}

/// 按真实行为管线口径估算规则产出量（纯计算，不写库、不调 LLM）。
///
/// 说明:
/// - 聚类口径（含 θ_nb 重试）由 `cluster_with_retry` 统一提供，与 θ_join 模拟一致。
/// - 对最终结果的每个簇执行 `refine_cluster`，再逐簇过 `quality_gate` 归类计数。
///
/// 参数:
/// - `samples`: 已向量化的行为样本。
/// - `behavior`: 应用本次覆盖后的行为配置（θ_nb/min_cluster_size/β 权重/孤立点比例上限）。
/// - `gate_config`: 质控闸门阈值（由 `RuleGenConfig::from` 从行为配置派生）。
fn estimate_pipeline(
    samples: &[BehaviorSample],
    behavior: &BehaviorConfig,
    gate_config: &RuleGenConfig,
) -> PipelineEstimate {
    let run = cluster_with_retry(samples, behavior);

    let mut sizes: Vec<usize> = run
        .result
        .clusters
        .iter()
        .map(|c| c.member_indices.len())
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));

    let refined: Vec<RefinedCluster> = run
        .result
        .clusters
        .iter()
        .map(|c| refine_cluster(samples, &c.member_indices, behavior.beta1, behavior.beta2))
        .collect();

    PipelineEstimate {
        retries_used: run.retries_used,
        effective_theta_nb: run.effective_theta_nb,
        cluster_count: run.result.cluster_count,
        sizes,
        outlier_ratio: run.result.outlier_ratio,
        gate: tally_quality_gate(&refined, gate_config),
    }
}

/// 校验本次计算覆盖的聚类参数。
///
/// 参数:
/// - `theta_nb`: 邻域相似度阈值，必须在 [0.0, 1.0]。
/// - `min_cluster_size`: 核心样本最小邻居数，必须 ≥ 1。
/// - `beta1` / `beta2`: 双通道权重，必须为非负有限值且 β1 + β2 ≤ 1.0。
///
/// 返回:
/// - `Ok(())`: 参数合法。
/// - `Err`: 首个非法参数（错误信息含参数名与当前取值）。
fn validate_cluster_params(
    theta_nb: f64,
    min_cluster_size: usize,
    beta1: f64,
    beta2: f64,
) -> anyhow::Result<()> {
    if !theta_nb.is_finite() || !(0.0..=1.0).contains(&theta_nb) {
        anyhow::bail!("--theta-nb 必须在 [0.0, 1.0] 内，当前值: {theta_nb}");
    }
    if min_cluster_size < 1 {
        anyhow::bail!("--min-cluster-size 必须 ≥ 1，当前值: {min_cluster_size}");
    }
    if !beta1.is_finite() || beta1 < 0.0 {
        anyhow::bail!("--beta1 必须为非负有限值，当前值: {beta1}");
    }
    if !beta2.is_finite() || beta2 < 0.0 {
        anyhow::bail!("--beta2 必须为非负有限值，当前值: {beta2}");
    }
    if beta1 + beta2 > 1.0 {
        anyhow::bail!(
            "--beta1 + --beta2 必须 ≤ 1.0（关键词通道权重 = 1 − β1 − β2 不可为负），当前和为: {}",
            beta1 + beta2
        );
    }
    Ok(())
}

/// 校验 θ_join 时序增量模拟参数。
///
/// 参数:
/// - `theta_join`: θ_join 档位列表（空 = 不启用模拟）；每个值必须在 [0.0, 1.0]。
/// - `split_ratio`: 前段事件占比，必须在 [0.1, 0.9]。
///
/// 返回:
/// - `Ok(())`: 参数合法（含空档位列表）。
/// - `Err`: 首个非法取值（错误信息含参数名与当前值）。
fn validate_incremental_params(theta_join: &[f64], split_ratio: f64) -> anyhow::Result<()> {
    for &value in theta_join {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            anyhow::bail!("--theta-join 必须在 [0.0, 1.0] 内，当前值: {value}");
        }
    }
    if !split_ratio.is_finite() || !(0.1..=0.9).contains(&split_ratio) {
        anyhow::bail!("--split-ratio 必须在 [0.1, 0.9] 内，当前值: {split_ratio}");
    }
    Ok(())
}

// =========================================================
// clusters 辅助：θ_join 时序增量模拟
// =========================================================

/// θ_join 时序增量模拟结果。
///
/// 职责:
/// - 汇总"留一"模拟：前段事件建簇为模板规则，后段事件按时间逐条喂入增量管线，
///   统计各 θ_join 档位的归簇/待定/新簇与全量参照一致性。
///
/// 状态:
/// - `Skipped`: 事件不足 2 条，无法切分前段/后段。
/// - `Done`: 模拟完成（外层 split/first/second + 逐档统计）。
enum IncrementalSimulation {
    /// 已跳过（附跳过原因）。
    Skipped { reason: String },
    /// 模拟完成。
    Done(IncrementalSimulationReport),
}

/// θ_join 时序增量模拟报告。
struct IncrementalSimulationReport {
    /// 前段事件占比（回显本次取值）。
    split_ratio: f64,
    /// 前段事件数（建簇）。
    first_count: usize,
    /// 后段事件数（逐条喂入增量管线）。
    second_count: usize,
    /// 各 θ_join 档位的统计（与请求档位一一对应、顺序一致）。
    by_theta_join: Vec<ThetaJoinSummary>,
}

/// 单个 θ_join 档位的模拟统计。
///
/// 字段约定:
/// - `assigned` / `assigned_rate`: 归入前段模板规则的后段事件数与其占后段总数的比例。
/// - `pending_remaining`: 模拟结束时待定池剩余事件数（含未成簇与已成簇未消费事件）。
/// - `new_clusters` / `new_cluster_sizes`: 待定池新成簇事件组数与规模（同一事件组按首见去重）。
/// - `low_confidence`: 新标记低置信事件数（模拟即时完成，通常为 0）。
/// - `decayed_rules`: 被证据衰减标记为应降级/失效的规则数（按规则 id 去重）。
/// - `drift_triggered`: 是否出现过漂移触发（逐条喂入批次 < 3，恒 false，保留字段对齐批次语义）。
/// - `agreement_checked` / `agreement_rate`: 与全量参照聚类的一致性对照
///   （分母仅含规则侧有全量映射的归簇事件）。
struct ThetaJoinSummary {
    theta_join: f64,
    assigned: usize,
    assigned_rate: f64,
    pending_remaining: usize,
    new_clusters: usize,
    new_cluster_sizes: Vec<usize>,
    low_confidence: usize,
    decayed_rules: usize,
    drift_triggered: bool,
    agreement_checked: usize,
    agreement_rate: Option<f64>,
}

/// 按比例计算前段事件数（纯函数）。
///
/// 参数:
/// - `total`: 事件总数。
/// - `ratio`: 前段占比（调用方已校验 ∈ [0.1, 0.9]）。
///
/// 返回:
/// - `Some(first_count)`: 前段条数，恒 ∈ [1, total−1]（两段各至少 1 条）。
/// - `None`: 事件总数 < 2，无法切分。
fn split_index_by_ratio(total: usize, ratio: f64) -> Option<usize> {
    if total < 2 {
        return None;
    }
    let scaled = (total as f64 * ratio).round() as usize;
    Some(scaled.clamp(1, total - 1))
}

/// 多数投票取全量簇标签（纯函数）。
///
/// 说明:
/// - 最高票并列时取较小标签，保证输出确定性。
///
/// 返回:
/// - `Some(label)`: 得票最多的标签。
/// - `None`: 空输入（无可投票标签）。
fn majority_vote_label(labels: &[usize]) -> Option<usize> {
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for &label in labels {
        *counts.entry(label).or_insert(0) += 1;
    }
    let mut ranked: Vec<(usize, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.first().map(|&(label, _)| label)
}

/// 判定一次增量归簇与全量参照标签是否一致（纯函数）。
///
/// 参数:
/// - `rule_full_cluster`: 归入规则对应的全量簇标签（前段簇成员在全量聚类中的多数投票）；
///   `None` = 规则侧无全量映射（不可判定）。
/// - `event_full_cluster`: 被归簇事件的全量簇标签；`None` = 事件在全量侧为孤立点。
///
/// 返回:
/// - `Some(true)`: 标签一致。
/// - `Some(false)`: 不一致（标签不同；或事件在全量侧孤立 = 误归）。
/// - `None`: 规则侧不可判定，不计入一致性分母。
fn judge_agreement(
    rule_full_cluster: Option<usize>,
    event_full_cluster: Option<usize>,
) -> Option<bool> {
    rule_full_cluster.map(|rule_label| event_full_cluster == Some(rule_label))
}

/// 由前段簇构造"无文本 Auto 规则"模板（纯函数）。
///
/// 说明:
/// - 每条规则对应前段一个簇，`id` 从 1 递增（规则顺序与簇顺序一致），供增量归簇使用。
/// - 规则无 reaction、无证据、置信度/稳定性为 0，仅携带簇情境中心；
///   不写库、不进入任何注入路径。
///
/// 参数:
/// - `persona_uid`: 规则所属 persona。
/// - `refined`: 前段提炼后的簇列表。
/// - `now`: 创建/更新时间（Unix 毫秒）。
fn build_template_rules(
    persona_uid: &str,
    refined: &[RefinedCluster],
    now: i64,
) -> Vec<BehaviorRule> {
    refined
        .iter()
        .enumerate()
        .map(|(idx, cluster)| BehaviorRule {
            id: idx as i64 + 1,
            persona_uid: persona_uid.to_string(),
            situation: cluster.situation.clone(),
            reaction: None,
            params: BehaviorParams::default(),
            avoid: Vec::new(),
            evidence: Vec::new(),
            confidence: 0.0,
            stability: 0.0,
            source: RuleSource::Auto,
            enabled: true,
            created_at: now,
            updated_at: now,
        })
        .collect()
}

/// 运行 θ_join 时序增量模拟（只读"留一"评估，不写库、不调 LLM）。
///
/// 流程:
/// 1. 事件按 `start` 升序稳定排序，按 `split_ratio` 切分前段（建簇）/后段（增量）。
/// 2. 前段用管线口径（含 θ_nb 重试）聚类并提炼，构造无文本 Auto 规则模板。
/// 3. 全量事件跑同一口径聚类，建立"事件 id → 全量簇 idx"参照标签，
///    并以模板规则成员的全量标签多数投票建立"规则 idx → 全量簇 idx"映射。
/// 4. 每个 θ_join 档位独立模拟：模板规则重新克隆、待定池独立，
///    后段事件逐条喂入 `compute_incremental_update`，累计归簇/新簇/低置信/衰减/漂移与一致性。
///
/// 说明:
/// - 本模拟的规则无 evidence，`compute_incremental_update` 的证据衰减会因总权重 0.0
///   低于阈值把全部模板规则标记为 decayed（该行为来自库函数，模拟不修改）；
///   `decayed_rules` 按规则 id 去重统计，不代表真实规则的衰减结论。
/// - 待定池 `advance` 不消费已成簇事件，后续轮次会重复返回同一批事件组；
///   `new_clusters` / `new_cluster_sizes` 按事件组首见去重，`pending_remaining` 保留池内全部事件。
/// - 逐条喂入时单批新事件 < 3，漂移检测分支不执行，`drift_triggered` 恒 false。
/// - 一致性对照仅在"规则侧有全量映射"时计入分母；事件在全量侧孤立（None）判为误归。
///
/// 参数:
/// - `persona_uid`: 目标 persona。
/// - `events`: 全量事件（与 `samples` 同索引、同顺序）。
/// - `samples`: 已向量化的全量样本。
/// - `behavior`: 应用本次覆盖后的行为配置（θ_join 按档位覆写）。
/// - `embedder`: 嵌入模型 provider（None → 纯关键词降级）。
/// - `theta_join_values`: θ_join 档位列表（每档独立模拟）。
/// - `split_ratio`: 前段事件占比。
///
/// 返回:
/// - `Skipped`: 事件不足 2 条（无法切分两段）。
/// - `Done`: 逐档统计报告。
async fn run_incremental_simulation(
    persona_uid: &str,
    events: &[MemoryEvent],
    samples: &[BehaviorSample],
    behavior: &BehaviorConfig,
    embedder: Option<&dyn EmbeddingProvider>,
    theta_join_values: &[f64],
    split_ratio: f64,
) -> anyhow::Result<IncrementalSimulation> {
    let Some(first_count) = split_index_by_ratio(events.len(), split_ratio) else {
        return Ok(IncrementalSimulation::Skipped {
            reason: format!(
                "事件数 {} 不足 2 条，前段/后段无法各保留至少 1 条，跳过模拟",
                events.len()
            ),
        });
    };

    // 事件按 start 升序稳定排序；samples 与 events 同索引，一并按同序取用
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by_key(|&idx| events[idx].start);
    let first_samples: Vec<BehaviorSample> = order[..first_count]
        .iter()
        .map(|&idx| samples[idx].clone())
        .collect();
    let second_events: Vec<&MemoryEvent> = order[first_count..]
        .iter()
        .map(|&idx| &events[idx])
        .collect();
    let second_count = second_events.len();

    // 前段建簇（与 pipeline 同口径）并提炼为模板规则
    let first_run = cluster_with_retry(&first_samples, behavior);
    let refined: Vec<RefinedCluster> = first_run
        .result
        .clusters
        .iter()
        .map(|cluster| {
            refine_cluster(
                &first_samples,
                &cluster.member_indices,
                behavior.beta1,
                behavior.beta2,
            )
        })
        .collect();

    // 全量参照标签：事件 id → 全量簇 idx（不在任何簇 = 孤立）
    let full_run = cluster_with_retry(samples, behavior);
    let mut full_label_by_event: HashMap<i64, usize> = HashMap::new();
    for (cluster_idx, cluster) in full_run.result.clusters.iter().enumerate() {
        for &sample_idx in &cluster.member_indices {
            if let Some(sample) = samples.get(sample_idx) {
                full_label_by_event.insert(sample.event_id, cluster_idx);
            }
        }
    }

    // 规则 idx → 全量簇 idx：前段簇成员的全量标签多数投票
    let mut rule_full_label: HashMap<usize, usize> = HashMap::new();
    for (rule_idx, cluster) in refined.iter().enumerate() {
        let labels: Vec<usize> = cluster
            .member_event_ids
            .iter()
            .filter_map(|event_id| full_label_by_event.get(event_id).copied())
            .collect();
        if let Some(label) = majority_vote_label(&labels) {
            rule_full_label.insert(rule_idx, label);
        }
    }

    let now = now_ms();
    let template_rules = build_template_rules(persona_uid, &refined, now);
    let rule_id_to_idx: HashMap<i64, usize> = template_rules
        .iter()
        .enumerate()
        .map(|(idx, rule)| (rule.id, idx))
        .collect();

    let mut by_theta_join = Vec::with_capacity(theta_join_values.len());
    for &theta_join in theta_join_values {
        // 每档独立：模板规则重新克隆（证据衰减是原地修改），待定池独立
        let mut cfg = behavior.clone();
        cfg.theta_join = theta_join;
        let mut rules = template_rules.clone();
        let mut pending = PendingPool::new(&cfg);

        let mut assigned = 0usize;
        let mut agreement_checked = 0usize;
        let mut agreement = 0usize;
        let mut seen_groups: HashSet<Vec<i64>> = HashSet::new();
        let mut new_cluster_sizes: Vec<usize> = Vec::new();
        let mut low_confidence = 0usize;
        let mut decayed_seen: HashSet<i64> = HashSet::new();
        let mut drift_triggered = false;

        for event in &second_events {
            let outcome = compute_incremental_update(
                std::slice::from_ref(*event),
                &mut rules,
                &mut pending,
                &cfg,
                embedder,
                now,
            )
            .await
            .context("计算增量更新失败")?;

            for &(event_id, rule_id) in &outcome.assigned {
                assigned += 1;
                let Some(&rule_idx) = rule_id_to_idx.get(&rule_id) else {
                    continue;
                };
                let rule_label = rule_full_label.get(&rule_idx).copied();
                let event_label = full_label_by_event.get(&event_id).copied();
                if let Some(agreed) = judge_agreement(rule_label, event_label) {
                    agreement_checked += 1;
                    if agreed {
                        agreement += 1;
                    }
                }
            }

            for group in &outcome.new_cluster_event_ids {
                let mut key = group.clone();
                key.sort_unstable();
                if seen_groups.insert(key) {
                    new_cluster_sizes.push(group.len());
                }
            }
            low_confidence += outcome.low_confidence_event_ids.len();
            decayed_seen.extend(outcome.decayed_rule_ids.iter().copied());
            drift_triggered |= outcome.drift_triggered;
        }

        let assigned_rate = assigned as f64 / second_count as f64;
        let agreement_rate = if agreement_checked > 0 {
            Some(agreement as f64 / agreement_checked as f64)
        } else {
            None
        };
        by_theta_join.push(ThetaJoinSummary {
            theta_join,
            assigned,
            assigned_rate,
            pending_remaining: pending.events.len(),
            new_clusters: new_cluster_sizes.len(),
            new_cluster_sizes,
            low_confidence,
            decayed_rules: decayed_seen.len(),
            drift_triggered,
            agreement_checked,
            agreement_rate,
        });
    }

    Ok(IncrementalSimulation::Done(IncrementalSimulationReport {
        split_ratio,
        first_count,
        second_count,
        by_theta_join,
    }))
}

/// 判断可选文本字段是否存在有效内容（None / 纯空白视为空）。
fn has_text(value: Option<&str>) -> bool {
    value.is_some_and(|s| !s.trim().is_empty())
}

/// 计算升序序列的百分位（线性插值）。
///
/// 参数:
/// - `sorted`: 升序排列的数值序列。
/// - `p`: 百分位（0.0..=1.0，越界自动 clamp）。
///
/// 返回:
/// - 位置 `p·(n−1)` 处的线性插值；空输入无定义，返回 0.0。
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let p = p.clamp(0.0, 1.0);
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// 簇形状汇总（最大簇占比与孤立点统计）。
///
/// 职责:
/// - 由各簇规模与样本总数推导 max_share / outlier_count / outlier_ratio，供输出与单测复用。
///
/// 字段约定:
/// - `max_share`: 最大簇成员数 / 样本总数（无样本 → 0.0）。
/// - `outlier_count`: 未入簇样本数（样本总数 − 各簇规模之和）。
/// - `outlier_ratio`: outlier_count / 样本总数（无样本 → 0.0）。
#[derive(Debug, Clone, PartialEq)]
struct ClusterShapeSummary {
    max_share: f64,
    outlier_count: usize,
    outlier_ratio: f64,
}

/// 汇总簇形状统计（纯函数）。
///
/// 参数:
/// - `sizes`: 各簇成员数（顺序无关，内部取最大值）。
/// - `total`: 参与聚类的样本总数（含孤立点）。
///
/// 返回:
/// - 最大簇占比与孤立点统计；`total = 0` 时全部为 0。
fn summarize_cluster_shapes(sizes: &[usize], total: usize) -> ClusterShapeSummary {
    if total == 0 {
        return ClusterShapeSummary {
            max_share: 0.0,
            outlier_count: 0,
            outlier_ratio: 0.0,
        };
    }
    let max_size = sizes.iter().copied().max().unwrap_or(0);
    let assigned: usize = sizes.iter().sum();
    let outlier_count = total.saturating_sub(assigned);
    ClusterShapeSummary {
        max_share: max_size as f64 / total as f64,
        outlier_count,
        outlier_ratio: outlier_count as f64 / total as f64,
    }
}

/// 全对融合相似度的分布统计。
///
/// 字段约定:
/// - `pairs`: 参与统计的样本对数 n·(n−1)/2。
/// - `min` / `p25` / `p50` / `p75` / `p90` / `max`: 升序分布的百分位（线性插值）。
/// - `mean`: 算术平均。
#[derive(Debug, Clone)]
struct SimilarityStats {
    pairs: usize,
    min: f64,
    p25: f64,
    p50: f64,
    p75: f64,
    p90: f64,
    max: f64,
    mean: f64,
}

/// 相似度分布的计算结果。
///
/// 职责:
/// - 区分「已算出分布」与「跳过（样本不足 / 对数超限）」两种状态，供输出层统一处理。
enum SimilarityDistribution {
    /// 完整的全对分布统计。
    Computed(SimilarityStats),
    /// 跳过计算（附跳过原因）。
    Skipped(String),
}

/// 计算全部样本对的融合相似度分布（纯计算，不写库）。
///
/// 参数:
/// - `samples`: 已向量化的行为样本。
/// - `beta1` / `beta2`: 三路融合权重（仅本次计算）。
///
/// 返回:
/// - `Computed`: 对数在 `MAX_SIMILARITY_PAIRS` 内且 ≥ 1 时的分布统计。
/// - `Skipped`: 样本不足 2 条（无可比较对），或对数超限（记 warn 后跳过）。
fn compute_similarity_stats(
    samples: &[BehaviorSample],
    beta1: f64,
    beta2: f64,
) -> SimilarityDistribution {
    let pair_count = samples
        .len()
        .saturating_mul(samples.len().saturating_sub(1))
        / 2;
    if pair_count > MAX_SIMILARITY_PAIRS {
        tracing::warn!(
            pairs = pair_count,
            max_pairs = MAX_SIMILARITY_PAIRS,
            "行为聚类样本对数超过上限，跳过相似度分布计算"
        );
        return SimilarityDistribution::Skipped(format!(
            "样本对数 {pair_count} 超过上限 {MAX_SIMILARITY_PAIRS}，已跳过相似度分布计算"
        ));
    }
    if pair_count == 0 {
        return SimilarityDistribution::Skipped("样本不足 2 条，无可比较样本对".to_string());
    }

    let mut sims: Vec<f64> = Vec::with_capacity(pair_count);
    for i in 0..samples.len() {
        for j in (i + 1)..samples.len() {
            sims.push(fused_similarity(&samples[i], &samples[j], beta1, beta2));
        }
    }
    sims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean = sims.iter().sum::<f64>() / sims.len() as f64;
    SimilarityDistribution::Computed(SimilarityStats {
        pairs: pair_count,
        min: percentile(&sims, 0.0),
        p25: percentile(&sims, 0.25),
        p50: percentile(&sims, 0.5),
        p75: percentile(&sims, 0.75),
        p90: percentile(&sims, 0.9),
        max: percentile(&sims, 1.0),
        mean,
    })
}

// =========================================================
// 单元测试（纯函数，不碰真实 DB / embedding）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::behavior::BehaviorSituation;

    /// `percentile`: 空输入返回 0.0，单元素恒为该值。
    #[test]
    fn percentile_handles_empty_and_single() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[0.42], 0.0), 0.42);
        assert_eq!(percentile(&[0.42], 1.0), 0.42);
    }

    /// `percentile`: 两元素线性插值（中点 = 均值），p=0/1 取端点。
    #[test]
    fn percentile_interpolates_linearly() {
        let data = [10.0, 20.0];
        assert_eq!(percentile(&data, 0.0), 10.0);
        assert_eq!(percentile(&data, 1.0), 20.0);
        assert_eq!(percentile(&data, 0.5), 15.0);
        assert_eq!(percentile(&data, 0.25), 12.5);
    }

    /// `summarize_cluster_shapes`: 最大簇占比与孤立点统计。
    #[test]
    fn summarize_cluster_shapes_counts_outliers_and_share() {
        // 3 + 1 簇、共 10 个样本：最大占比 0.3，孤立点 6 个（60%）
        let shape = summarize_cluster_shapes(&[3, 1], 10);
        assert!((shape.max_share - 0.3).abs() < 1e-12);
        assert_eq!(shape.outlier_count, 6);
        assert!((shape.outlier_ratio - 0.6).abs() < 1e-12);

        // 全部入簇：孤立点为 0
        let full = summarize_cluster_shapes(&[4, 2], 6);
        assert!((full.max_share - 4.0 / 6.0).abs() < 1e-12);
        assert_eq!(full.outlier_count, 0);
        assert_eq!(full.outlier_ratio, 0.0);

        // 无样本：全部为 0
        let empty = summarize_cluster_shapes(&[], 0);
        assert_eq!(empty.max_share, 0.0);
        assert_eq!(empty.outlier_count, 0);
        assert_eq!(empty.outlier_ratio, 0.0);
    }

    /// `tally_quality_gate`: 逐簇判定按 Pass / 证据不足 / n_eff 不足 / valence 方差超限归类。
    #[test]
    fn tally_quality_gate_classifies_verdicts() {
        let config = RuleGenConfig {
            min_evidence: 5,
            min_n_eff: 5,
            valence_std_limit: 0.5,
            ..RuleGenConfig::default()
        };
        let clusters = vec![
            // 三项全达标 → Pass
            test_refined_cluster(10, 10.0, 0.1),
            // 证据量不足（按闸门顺序先于 n_eff / 方差判定）
            test_refined_cluster(4, 1.0, 0.9),
            // n_eff 不足
            test_refined_cluster(10, 2.0, 0.1),
            // valence 标准差超限
            test_refined_cluster(10, 10.0, 0.9),
        ];
        let tally = tally_quality_gate(&clusters, &config);
        assert_eq!(
            tally,
            GateTally {
                pass: 1,
                low_evidence: 1,
                low_neff: 1,
                high_valence_variance: 1,
            }
        );
    }

    /// `summarize_counterfactual`: 规模降序、3 成员簇计数与回收样本数。
    #[test]
    fn summarize_counterfactual_counts_small_clusters_and_recoveries() {
        // 反事实 4 簇（3/3/2/1 共 9 样本）+ 7 孤立点；原始孤立 14 → 回收 7
        let summary = summarize_counterfactual(2, &[1, 3, 2, 3], 16, 14);
        assert_eq!(summary.min_cluster_size, 2);
        assert_eq!(summary.count, 4);
        assert_eq!(summary.sizes, vec![3, 3, 2, 1]);
        assert_eq!(summary.three_member_clusters, 2);
        assert_eq!(summary.outlier_count, 7);
        assert!((summary.outlier_ratio - 7.0 / 16.0).abs() < 1e-12);
        assert_eq!(summary.recovered_samples, 7);

        // 无样本：孤立点统计为 0，不 panic
        let empty = summarize_counterfactual(1, &[], 0, 0);
        assert_eq!(empty.count, 0);
        assert_eq!(empty.outlier_count, 0);
        assert_eq!(empty.outlier_ratio, 0.0);
        assert_eq!(empty.recovered_samples, 0);
    }

    /// `split_index_by_ratio`: 前段条数按比例取整后夹在 [1, total−1]（两段非空）。
    #[test]
    fn split_index_by_ratio_keeps_both_segments_non_empty() {
        assert_eq!(split_index_by_ratio(10, 0.8), Some(8));
        assert_eq!(split_index_by_ratio(5, 0.8), Some(4));
        assert_eq!(split_index_by_ratio(10, 0.1), Some(1), "下界夹取");
        assert_eq!(split_index_by_ratio(2, 0.9), Some(1), "上界夹取");
        assert_eq!(split_index_by_ratio(1, 0.8), None, "单条无法切分");
        assert_eq!(split_index_by_ratio(0, 0.8), None, "空输入无法切分");
    }

    /// `judge_agreement`: 标签一致 / 不一致 / 全量孤立误归 / 规则侧不可判定。
    #[test]
    fn judge_agreement_classifies_matches_and_mismatches() {
        assert_eq!(judge_agreement(Some(2), Some(2)), Some(true));
        assert_eq!(judge_agreement(Some(2), Some(3)), Some(false));
        assert_eq!(
            judge_agreement(Some(2), None),
            Some(false),
            "全量侧孤立 = 误归"
        );
        assert_eq!(
            judge_agreement(None, Some(1)),
            None,
            "规则侧无映射 → 不计分母"
        );
        assert_eq!(judge_agreement(None, None), None);
    }

    /// `majority_vote_label`: 取最高票；并列时取较小标签；空输入无标签。
    #[test]
    fn majority_vote_label_prefers_count_then_smallest_label() {
        assert_eq!(majority_vote_label(&[1, 1, 2]), Some(1));
        assert_eq!(majority_vote_label(&[2, 2, 1]), Some(2));
        assert_eq!(majority_vote_label(&[3, 1]), Some(1), "并列取较小标签");
        assert_eq!(majority_vote_label(&[]), None);
    }

    /// 构造提炼簇（测试辅助）：仅设置闸门判定所需字段。
    fn test_refined_cluster(sample_count: usize, n_eff: f64, valence_std: f64) -> RefinedCluster {
        let mut situation = BehaviorSituation::empty();
        situation.sample_count = sample_count;
        situation.valence_std = valence_std;
        RefinedCluster {
            situation,
            n_eff,
            cohesion: 1.0,
            quality: 1.0,
            member_event_ids: Vec::new(),
            member_events: Vec::new(),
        }
    }
}
