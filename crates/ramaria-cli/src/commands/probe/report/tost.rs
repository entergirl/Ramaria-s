//! crates/ramaria-cli/src/commands/probe/report/tost.rs - 探针 report 等效性结论文案
//!
//! 设计特点:
//! - 只负责把等效性判定结果转成报告措辞，数值计算在评测层统计模块
//! - 区分"已证等效"与"样本量不足以判定"两种未显著情形
//! - 等效分支报出等效边界与合并 SD 口径的 |d_av|，便于人工核对边界

/// 未达显著差异时的结论文案：区分"已证等效"与"样本量不足以判定"。
///
/// 说明:
/// - 显著与等效互斥，本函数只在 `significant == false` 时被调用；
/// - 等效分支报出等效边界（原始差分量纲）与 |d_av|，便于人工核对边界是否合理；
/// - 不可判定分支同时报 p_fdr 与 tost_p，指向"提高重复次数/题量"的下一步。
pub(crate) fn equivalence_annotation(
    type_label: &str,
    p_fdr: f64,
    tost_p: f64,
    equiv_bound: f64,
    cohens_d_pooled: f64,
    cohens_d: f64,
    equivalent: bool,
) -> String {
    if equivalent {
        format!(
            "{type_label}：等效（TOST p={tost_p:.3} < 0.05，等效边界 Δ=±{equiv_bound:.4}，\
             |d_av|={:.2}）→ 可判定该对照无实质净增量",
            cohens_d_pooled.abs()
        )
    } else {
        format!(
            "{type_label}：既未达显著差异、亦未达统计等效（p_fdr={p_fdr:.3}, tost_p={tost_p:.3}, \
             |d|={cohens_d:.2}）→ 样本量不足以判定"
        )
    }
}
