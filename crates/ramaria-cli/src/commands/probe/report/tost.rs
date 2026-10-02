//! crates/ramaria-cli/src/commands/probe/report/tost.rs - 探针 report 等效性检验（TOST）
//!
//! 设计特点:
//! - 双单侧 t 检验，补上显著性框架无法证明零净增量的盲区
//! - 等效边界取 |d_av|=0.3（合并 SD 口径），配合三态判定标注
//! - student_t_cdf / 不完全 Beta 与 ln_gamma 为纯数值实现

/// TOST 等效性检验结果。
#[derive(Debug, Clone, Copy)]
pub(crate) struct TostOutcome {
    /// 原始差分量纲的等效边界（= bound_d × sd_av）
    pub bound: f64,
    /// TOST p 值（max(两个单侧 p)）
    pub p: f64,
    /// 是否可判定等效（p < 0.05）
    pub equivalent: bool,
}

/// 配对样本的 TOST 等效性检验（t 分布，df = n − 1）。
///
/// 参数:
/// - `diffs`: 配对差分样本（ablated − base）。
/// - `base` / `ablated`: 配对分数向量（用于合并 SD 计算等效边界）。
/// - `bound_d`: 标准化等效边界。
///
/// 返回:
/// - n<2 或 SD 非法 → None（样本不足，调用方按不可判定处理）。
pub(crate) fn tost_equivalence(
    diffs: &[f64],
    base: &[f64],
    ablated: &[f64],
    bound_d: f64,
) -> Option<TostOutcome> {
    let n = diffs.len();
    if n < 2 {
        return None;
    }
    let n_f = n as f64;
    let mean = diffs.iter().sum::<f64>() / n_f;
    let var = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n_f - 1.0);
    let sd = var.sqrt();
    if sd < 1e-12 {
        // 差分为常数：无抽样波动，无法做 t 检验
        return None;
    }
    // 合并 SD（两条件离散度）作为标准化标尺；只取前 n 项，
    // 保证与 `diffs` 一一配对的样本对齐（正常调用路径三者等长）。
    let var_of = |xs: &[f64]| {
        let m = xs.iter().take(n).sum::<f64>() / n_f;
        xs.iter().take(n).map(|x| (x - m) * (x - m)).sum::<f64>() / (n_f - 1.0)
    };
    let sd_av = ((var_of(base) + var_of(ablated)) / 2.0).sqrt();
    if sd_av < 1e-12 {
        return None;
    }
    let bound = bound_d * sd_av;
    let se = sd / n_f.sqrt();
    let df = n_f - 1.0;
    // H0_low: mean <= -bound（上侧检验）；H0_high: mean >= +bound（下侧检验）
    let t_low = (mean + bound) / se;
    let t_high = (mean - bound) / se;
    let p_low = 1.0 - student_t_cdf(t_low, df);
    let p_high = student_t_cdf(t_high, df);
    let p = p_low.max(p_high).clamp(0.0, 1.0);
    Some(TostOutcome {
        bound,
        p,
        equivalent: p < 0.05,
    })
}

/// 学生氏 t 分布累积分布函数。
///
/// 说明:
/// - 用正则化不完全贝塔函数计算（含 Lanczos ln Γ 与连分数），精度足以支撑
///   p 值判定（相对误差 <1e-6 量级）。
/// - df <= 0 或 t 为 NaN → 返回 0.5（退化，不 panic）。
pub(crate) fn student_t_cdf(t: f64, df: f64) -> f64 {
    if !t.is_finite() || df <= 0.0 {
        return 0.5;
    }
    let x = df / (df + t * t);
    let ib = betai(df / 2.0, 0.5, x);
    if t > 0.0 { 1.0 - 0.5 * ib } else { 0.5 * ib }
}

/// 正则化不完全贝塔函数 I_x(a,b)（Numerical Recipes 6.4）。
pub(crate) fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_bt = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    let bt = ln_bt.exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * betacf(a, b, x) / a
    } else {
        1.0 - bt * betacf(b, a, 1.0 - x) / b
    }
}

/// 不完全贝塔连分数（Numerical Recipes 6.4 betacf）。
pub(crate) fn betacf(a: f64, b: f64, x: f64) -> f64 {
    const MAX_ITER: usize = 200;
    const EPS: f64 = 3.0e-12;
    const FPMIN: f64 = 1.0e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_ITER {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;
        let aa = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// ln Γ(x)（Lanczos 近似，Numerical Recipes 6.1）。
pub(crate) fn ln_gamma(x: f64) -> f64 {
    const COF: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let mut y = x;
    let mut tmp = x + 5.5;
    tmp -= (x + 0.5) * tmp.ln();
    let mut ser = 1.000000000190015;
    for c in COF {
        y += 1.0;
        ser += c / y;
    }
    -tmp + (2.5066282746310005 * ser / x).ln()
}

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
