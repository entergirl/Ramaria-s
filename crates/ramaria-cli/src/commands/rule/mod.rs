//! crates/ramaria-cli/src/commands/rule/mod.rs - 行为规则管理命令模块入口
//!
//! 设计特点:
//! - 子命令遵循动词词表：list/show/import/edit/enable/disable/delete/evidence/relearn
//! - clusters 为只读统计：读事件 → 双通道向量化 → 密度聚类 → 簇结构 / 相似度分布
//! - 全部支持全局 `--json` 信封；stdout 只输出数据
//! - delete 为破坏性操作：交互确认 / 非 TTY 或 `--yes` 自动通过
//! - evidence 展示规则 → 事件 → 原文溯源链（只含结构化字段，原文不落日志）
//! - 子模块按域拆分，对外名称在此逐项 re-export（`commands::rule::X` 路径不变）

mod clusters;
mod incremental;
mod manage;

use clusters::run_clusters;
use manage::{
    run_delete, run_edit, run_evidence, run_import, run_list, run_relearn, run_set_enabled,
    run_show,
};
use ramaria_service::Engine;
use std::sync::Arc;

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
pub(crate) const DEFAULT_RULE_PERSONA: &str = "rama-0001";

/// 运行 rule 子命令分发。
///
/// 参数:
/// - `engine`: 服务层引擎引用。
/// - `cmd`: Rule 子命令。
/// - `json`: JSON 信封输出。
/// - `yes`: 自动确认所有确认点（delete 等）。
pub async fn run(engine: &Arc<Engine>, cmd: RuleCmd, json: bool, yes: bool) -> anyhow::Result<()> {
    match cmd {
        RuleCmd::List {
            persona,
            limit,
            offset,
        } => run_list(engine, persona, limit, offset, json).await,
        RuleCmd::Show { id } => run_show(engine, id, json).await,
        RuleCmd::Import { file, persona } => run_import(engine, &file, persona, json).await,
        RuleCmd::Edit {
            id,
            reaction,
            avoid,
        } => run_edit(engine, id, reaction, avoid, json).await,
        RuleCmd::Enable { id } => run_set_enabled(engine, id, true, json).await,
        RuleCmd::Disable { id } => run_set_enabled(engine, id, false, json).await,
        RuleCmd::Delete { id, force } => run_delete(engine, id, force, json, yes).await,
        RuleCmd::Evidence { id } => run_evidence(engine, id, json).await,
        RuleCmd::Relearn { persona } => run_relearn(engine, persona, json).await,
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
                engine,
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

#[cfg(test)]
pub(crate) use clusters::{GateTally, summarize_counterfactual, tally_quality_gate};
#[cfg(test)]
pub(crate) use incremental::{
    judge_agreement, majority_vote_label, percentile, split_index_by_ratio,
    summarize_cluster_shapes,
};

#[cfg(test)]
mod tests;
