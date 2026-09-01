//! crates/ramaria-memory/src/style/mod.rs - 表达层风格统计模块（A3）
//!
//! 设计特点:
//! - 五维风格指标统计（stat.rs）：口癖词/句式句长/标点/情感表达/话题词汇偏好
//! - 全局基线池 + 二项 z 检验（baseline.rs）：|z|≥2 且频次≥5 且 n_p≥200
//! - 自动规则生成（rule_gen.rs）：模板优先 + LLM 翻译增强（`[style].auto_translate`）
//! - 数据不足（n_p<200）标注不生成、静默跳过（回归红线 1）
//! - 安全约束：统计参数与基线池不含原文消息文本（隐私红线）

pub mod baseline;
pub mod rule_gen;
pub mod stat;

pub use baseline::{BaselinePool, MetricKey, PersonaFreq, is_significant, z_test};
pub use rule_gen::{
    CatchphraseHit, StyleSignificant, analyze_significance, generate_style_rule,
    render_template_rule,
};
pub use stat::StyleStats;
