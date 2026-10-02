//! crates/ramaria-cli/src/commands/probe/tests/scoring.rs - 探针 probe 消融 Profile 与自动评分 单元测试
//!
//! 设计特点:
//! - 消融档位 Profile（B0/B1/F0~F4/S_*/I_*）注入闸门与序列化兼容
//! - 事实 / 语气 / 情感三维判据（含事实维长度中性多判据）
//! - --repeat 逐轮评分聚合与旧格式向后兼容

use super::super::evaluate::FactItemScore;
use super::super::evaluate::VariantEvaluation;
use super::super::evaluate::aggregate_round_dimension_scores;
use super::super::evaluate::content_bigrams;
use super::super::evaluate::fact_point_score;
use super::super::evaluate::keyword_hit_norm_score;
use super::super::evaluate::reference_clauses;
use super::super::evaluate::score_emotion_item;
use super::super::types::VariantOverrides;
use super::super::*;
use ramaria_core::traits::EmbeddingModelInfo;
use ramaria_core::traits::EmbeddingProvider;
use std::sync::Arc;

/// 全部 15 个名称可解析且往返一致；未知名称返回 None。
#[test]
fn ablation_profile_parse_roundtrip_all_names() {
    assert_eq!(ABLATION_PROFILE_NAMES.len(), 15);
    for name in ABLATION_PROFILE_NAMES {
        let p = AblationProfile::parse_name(name).unwrap_or_else(|| panic!("名称 {name} 应可解析"));
        assert_eq!(p.name(), name, "解析后名称往返一致");
    }
    assert!(AblationProfile::parse_name("unknown").is_none());
    assert!(AblationProfile::parse_name("").is_none());
    assert!(AblationProfile::parse_name("f0").is_none(), "大小写敏感");
}

/// ablation_variants：15 档、id=Profile 名、utt 取定稿基准、ablation 回显。
#[test]
fn ablation_variants_shape_and_baseline_utt() {
    let variants = ablation_variants();
    assert_eq!(variants.len(), 15);
    for v in &variants {
        assert_eq!(
            v.ablation.as_deref(),
            Some(v.id.as_str()),
            "ablation 与 id 一致"
        );
        assert_eq!(v.theta_gap_minutes, 10, "消融档位 utt 取定稿基准");
        assert_eq!(v.max_msgs_per_block, 80);
        assert_eq!(v.retrieve_top_k, 3);
        assert!(v.description.contains("[消融]"));
    }
    // 覆盖 B0/B1/F0/F1~F4/S_* 全集
    let ids: Vec<&str> = variants.iter().map(|v| v.id.as_str()).collect();
    for name in ABLATION_PROFILE_NAMES {
        assert!(ids.contains(&name), "缺少档位 {name}");
    }
}

/// B0 无记忆注入：闸门全关；B1 压缩摘要基座：仅 memory_rag 开。
#[test]
fn ablation_profile_b0_b1_gates() {
    let mut cfg = ramaria_core::config::RamariaConfig::default();
    AblationProfile::B0.apply_to(&mut cfg);
    assert!(!cfg.injection.behavior);
    assert!(!cfg.injection.knowledge);
    assert!(!cfg.injection.speaking_style);
    assert!(!cfg.injection.examples);
    assert!(!cfg.injection.utt);
    assert!(!cfg.injection.narrative);
    assert!(!cfg.injection.bridge);
    assert!(!cfg.injection.memory_rag, "B0 关闭 RAG 相关记忆");

    let mut cfg = ramaria_core::config::RamariaConfig::default();
    AblationProfile::B1.apply_to(&mut cfg);
    assert!(cfg.injection.memory_rag, "B1 保留 RAG 摘要基座");
    assert!(!cfg.injection.behavior);
    assert!(!cfg.injection.knowledge);
    assert!(!cfg.injection.speaking_style);
    assert!(!cfg.injection.examples);
    assert!(!cfg.injection.utt);
    assert!(!cfg.injection.narrative);
    assert!(!cfg.injection.bridge);
}

/// F0 全开（与 None 等同）；F1~F4 在全开基础上只关对应层。
#[test]
fn ablation_profile_f0_to_f4_gates() {
    let mut cfg = ramaria_core::config::RamariaConfig {
        injection: ramaria_core::config::InjectionGate::all_off(),
        ..Default::default()
    };
    AblationProfile::F0.apply_to(&mut cfg);
    assert!(cfg.injection.behavior && cfg.injection.memory_rag && cfg.injection.utt);
    assert!(cfg.injection.narrative && cfg.injection.bridge);

    let mut f1 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F1.apply_to(&mut f1);
    assert!(!f1.injection.behavior, "F1 关行为层");
    assert!(f1.injection.knowledge && f1.injection.memory_rag && f1.injection.utt);
    assert!(f1.injection.narrative && f1.injection.bridge && f1.injection.examples);

    let mut f2 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F2.apply_to(&mut f2);
    assert!(!f2.injection.knowledge, "F2 关知识层");
    assert!(f2.injection.behavior && f2.injection.memory_rag);

    let mut f3 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F3.apply_to(&mut f3);
    assert!(!f3.injection.speaking_style, "F3 关表达层（风格）");
    assert!(!f3.injection.examples, "F3 关表达层（示例）");
    assert!(!f3.injection.utt, "F3 关表达层（原文样例）");
    assert!(f3.injection.behavior && f3.injection.knowledge);
    assert!(f3.injection.narrative && f3.injection.bridge && f3.injection.memory_rag);

    let mut f4 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F4.apply_to(&mut f4);
    assert!(!f4.injection.narrative, "F4 关脉络（近期脉络）");
    assert!(!f4.injection.bridge, "F4 关脉络（桥接）");
    assert!(f4.injection.utt && f4.injection.behavior && f4.injection.knowledge);
    assert!(f4.injection.speaking_style && f4.injection.examples && f4.injection.memory_rag);
}

/// S_* 替代对照：去掉 RAG 摘要基座，仅开目标专属层（对照 B1 测单层替代能力）。
#[test]
fn ablation_profile_s_group_gates() {
    let mut sb = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SBehavior.apply_to(&mut sb);
    assert!(sb.injection.behavior, "S_behavior 开行为层");
    assert!(
        !sb.injection.memory_rag,
        "S_behavior 为替代对照：应去掉 RAG 摘要基座"
    );
    assert!(!sb.injection.knowledge && !sb.injection.speaking_style);
    assert!(!sb.injection.examples && !sb.injection.utt);
    assert!(!sb.injection.narrative && !sb.injection.bridge);

    let mut sk = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SKnowledge.apply_to(&mut sk);
    assert!(sk.injection.knowledge);
    assert!(!sk.injection.behavior && !sk.injection.memory_rag);

    let mut se = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SExpression.apply_to(&mut se);
    assert!(!se.injection.memory_rag, "S_expression 去掉 RAG 摘要基座");
    assert!(se.injection.speaking_style && se.injection.examples && se.injection.utt);
    assert!(!se.injection.behavior && !se.injection.knowledge);
    assert!(!se.injection.narrative && !se.injection.bridge);

    let mut sn = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SNarrative.apply_to(&mut sn);
    assert!(!sn.injection.memory_rag, "S_narrative 去掉 RAG 摘要基座");
    assert!(sn.injection.narrative && sn.injection.bridge);
    assert!(!sn.injection.utt && !sn.injection.behavior && !sn.injection.knowledge);
    assert!(!sn.injection.speaking_style && !sn.injection.examples);
}

/// I_* 净增量对照：保留 B1 RAG 基座（memory_rag），仅叠加目标专属层。
#[test]
fn ablation_profile_i_group_gates() {
    let mut ib = ramaria_core::config::RamariaConfig::default();
    AblationProfile::IBehavior.apply_to(&mut ib);
    assert!(ib.injection.memory_rag, "I_behavior 保留 B1 RAG 基座");
    assert!(ib.injection.behavior, "I_behavior 叠加行为层");
    assert!(!ib.injection.knowledge && !ib.injection.speaking_style);
    assert!(!ib.injection.examples && !ib.injection.utt);
    assert!(!ib.injection.narrative && !ib.injection.bridge);

    let mut ik = ramaria_core::config::RamariaConfig::default();
    AblationProfile::IKnowledge.apply_to(&mut ik);
    assert!(ik.injection.memory_rag && ik.injection.knowledge);
    assert!(!ik.injection.behavior);

    let mut ie = ramaria_core::config::RamariaConfig::default();
    AblationProfile::IExpression.apply_to(&mut ie);
    assert!(ie.injection.memory_rag, "I_expression 保留 B1 RAG 基座");
    assert!(ie.injection.speaking_style && ie.injection.examples && ie.injection.utt);
    assert!(!ie.injection.behavior && !ie.injection.knowledge);
    assert!(!ie.injection.narrative && !ie.injection.bridge);

    let mut inn = ramaria_core::config::RamariaConfig::default();
    AblationProfile::INarrative.apply_to(&mut inn);
    assert!(inn.injection.memory_rag, "I_narrative 保留 B1 RAG 基座");
    assert!(inn.injection.narrative && inn.injection.bridge);
    assert!(!inn.injection.utt && !inn.injection.behavior && !inn.injection.knowledge);
    assert!(!inn.injection.speaking_style && !inn.injection.examples);
}

/// ProbeVariant serde 向后兼容：旧数据集（无 ablation 字段）→ None；
/// 带 ablation 的档位 roundtrip 保留该字段。
#[test]
fn probe_variant_ablation_serde_backcompat() {
    // 旧格式：无 ablation 字段 → 反序列化为 None。
    let old = r#"{"id":"baseline","description":"对照基准","theta_gap_minutes":10,"max_msgs_per_block":80,"retrieve_top_k":3}"#;
    let parsed: ProbeVariant = serde_json::from_str(old).unwrap();
    assert!(parsed.ablation.is_none(), "旧数据集 ablation 应为 None");

    // 新格式：ablation 存在则保留。
    let v = ProbeVariant {
        id: "F1".to_string(),
        description: "d".to_string(),
        theta_gap_minutes: 10,
        max_msgs_per_block: 80,
        retrieve_top_k: 3,
        ablation: Some("F1".to_string()),
        overrides: VariantOverrides::default(),
    };
    let json = serde_json::to_string(&v).unwrap();
    assert!(
        json.contains("\"ablation\":\"F1\""),
        "ablation 应序列化: {json}"
    );
    let back: ProbeVariant = serde_json::from_str(&json).unwrap();
    assert_eq!(back.ablation.as_deref(), Some("F1"));

    // ablation=None 序列化时省略该键（保持旧产物最小差异）。
    let plain = ProbeVariant {
        ablation: None,
        ..v
    };
    let plain_json = serde_json::to_string(&plain).unwrap();
    assert!(
        !plain_json.contains("ablation"),
        "None ablation 应省略: {plain_json}"
    );
}

/// 档位参数覆盖 serde：旧数据集无 `overrides` 键 → 默认全 None（行为等价）；
/// 带覆盖的档位往返保留；空覆盖序列化时省略该键。
#[test]
fn variant_overrides_serde_roundtrip_and_backcompat() {
    // 旧格式（无 overrides）→ 默认空
    let old = r#"{"id":"baseline","description":"d","theta_gap_minutes":10,
        "max_msgs_per_block":80,"retrieve_top_k":3}"#;
    let parsed: ProbeVariant = serde_json::from_str(old).unwrap();
    assert!(parsed.overrides.is_empty(), "旧数据集 overrides 应为空");

    // 新格式：带覆盖 → 往返保留；序列化含 overrides 键
    let v = ProbeVariant {
        id: "p_rag_mem_10".to_string(),
        description: "rag_max_memories=10".to_string(),
        theta_gap_minutes: 10,
        max_msgs_per_block: 80,
        retrieve_top_k: 3,
        ablation: Some("B1".to_string()),
        overrides: VariantOverrides {
            rag_max_memories: Some(10),
            rag_max_summary_chars: None,
            knowledge_retrieve_top_k: None,
            knowledge_retrieve_threshold: Some(0.3),
        },
    };
    let json = serde_json::to_string(&v).unwrap();
    assert!(
        json.contains("\"rag_max_memories\":10"),
        "非空覆盖应序列化: {json}"
    );
    let back: ProbeVariant = serde_json::from_str(&json).unwrap();
    assert_eq!(back.overrides.rag_max_memories, Some(10));
    assert_eq!(back.overrides.knowledge_retrieve_threshold, Some(0.3));
    assert_eq!(back.overrides.rag_max_summary_chars, None);

    // 空覆盖序列化省略该键（旧产物最小差异）
    let plain = ProbeVariant {
        overrides: VariantOverrides::default(),
        ..v
    };
    let plain_json = serde_json::to_string(&plain).unwrap();
    assert!(
        !plain_json.contains("overrides"),
        "空覆盖应省略: {plain_json}"
    );
}

/// F0 档位（ablation="F0"）注入闸门全开——与 ablation=None 行为一致。
#[test]
fn ablation_f0_equivalent_to_none() {
    let mut with_f0 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F0.apply_to(&mut with_f0);
    let default_cfg = ramaria_core::config::RamariaConfig::default();
    assert!(
        with_f0.injection.behavior == default_cfg.injection.behavior
            && with_f0.injection.memory_rag == default_cfg.injection.memory_rag,
        "F0 闸门应与默认全开一致"
    );
}

// =========================================================
// emotion 第三维
// =========================================================

/// 情感线索判定：负面/正面触发词命中；中性消息不命中。
#[test]
fn emotion_cue_detection_cases() {
    assert!(has_emotion_cue("今天被领导骂了，很难过"));
    assert!(has_emotion_cue("我好生气，想投诉"));
    assert!(has_emotion_cue("收到 offer 了，太开心了"));
    assert!(!has_emotion_cue("周末一起去爬山吗"));
    assert!(!has_emotion_cue("请问这个功能怎么用"));
    assert!(has_negative_cue("很担心") && !has_positive_cue("很担心"));
    assert!(!has_negative_cue("太开心了") && has_positive_cue("太开心了"));
}

/// rubric：负面情境 + 充分安慰 → 1.0；1 个标记 → 0.5；无标记 → 0.0。
#[test]
fn emotion_rubric_negative_situation() {
    let q = "今天被领导当众批评，好难过";
    // 充分安慰：命中多个安慰/共情标记
    let full = score_emotion_item("别难过，我理解你，会好的，先深呼吸", q);
    assert_eq!(full.score, 1.0);
    assert!(full.situation_negative && !full.situation_positive);
    // 单标记：部分回应
    let partial = score_emotion_item("别担心，睡一觉就好了", q);
    assert_eq!(partial.score, 0.5);
    assert_eq!(partial.marker_hit, 1);
    // 无标记：冷漠回应
    let cold = score_emotion_item("这个方案本身就有问题，明天重写吧", q);
    assert_eq!(cold.score, 0.0);
    // 空回复
    let empty = score_emotion_item("", q);
    assert_eq!(empty.score, 0.0);
}

/// rubric：正面情境 + 分享喜悦 → 1.0；单标记 → 0.5；无 → 0.0。
#[test]
fn emotion_rubric_positive_situation() {
    let q = "我升职了，太开心了";
    let full = score_emotion_item("太好了，真棒，恭喜你！这是你应得的", q);
    assert_eq!(full.score, 1.0);
    assert!(full.situation_positive && !full.situation_negative);
    let partial = score_emotion_item("嗯，不错", q);
    assert_eq!(partial.score, 0.5);
    let cold = score_emotion_item("下次注意保持", q);
    assert_eq!(cold.score, 0.0);
}

/// 中性情境：两类标记合计弱判定。
#[test]
fn emotion_rubric_neutral_situation() {
    let q = "帮我看看这段代码";
    let score = score_emotion_item("别担心，我帮你看看，一起加油", q);
    assert_eq!(score.score, 1.0, "中性情境按共情标记合计");
    assert!(!score.situation_negative && !score.situation_positive);
}

// =========================================================
// --repeat 逐轮评分聚合
// =========================================================

/// 构造一个含单条 fact 题的轮次结果。
fn fact_round(reply: &str) -> ProbeVariantResult {
    ProbeVariantResult {
        variant_id: "v1".to_string(),
        description: "d".to_string(),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        runs: vec![ProbeRunItem {
            item_id: "fact-0001".to_string(),
            dimension: "fact".to_string(),
            question: "还记得「团子」吗？".to_string(),
            reply: reply.to_string(),
            metrics: ProbeMetrics {
                reply_chars: reply.chars().count(),
                elapsed_ms: 1,
            },
            error: None,
        }],
        failed_count: 0,
    }
}

/// 空 rounds → 无聚合记录。
#[tokio::test]
async fn aggregate_round_scores_empty_returns_none() {
    let agg = aggregate_round_dimension_scores(&[], &None, None, None, 0).await;
    assert!(agg.is_empty());
}

/// 三轮回复与 golden 完全一致 → fact 三口径轮均分恒 1.0，n=3、std=0、CI 退化。
#[tokio::test]
async fn aggregate_round_scores_pools_round_means() {
    let reference = "用户去年收养了一只猫，取名团子";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let rounds: Vec<ProbeVariantResult> = vec![
        fact_round(reference),
        fact_round(reference),
        fact_round(reference),
    ];
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden), 0).await;
    assert_eq!(agg.len(), 3, "事实维三口径各一条聚合");
    let fact = agg
        .iter()
        .find(|d| d.dimension == "fact")
        .expect("应有 fact 聚合");
    assert_eq!(fact.n, 3, "有效轮数 = 3");
    assert!((fact.mean - 1.0).abs() < 1e-9, "满分均值应为 1.0");
    assert_eq!(fact.std, 0.0);
    assert!((fact.ci95_low - 1.0).abs() < 1e-9);
    assert!((fact.ci95_high - 1.0).abs() < 1e-9);
}

/// 三轮回复质量不同 → 轮均分存在波动，mean 介于 (0,1)，std > 0，CI 有效。
#[tokio::test]
async fn aggregate_round_scores_captures_variation() {
    let reference = "用户去年收养了一只猫，取名团子";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let rounds: Vec<ProbeVariantResult> = vec![
        fact_round(reference),
        fact_round("不太记得了"),
        fact_round(reference),
    ];
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden), 0).await;
    let fact = agg
        .iter()
        .find(|d| d.dimension == "fact")
        .expect("应有 fact 聚合");
    assert_eq!(fact.n, 3);
    assert!(fact.mean > 0.0 && fact.mean < 1.0, "波动后均值应介于 0..1");
    assert!(fact.std > 0.0, "质量波动应产生正 std");
    assert!(fact.ci95_low < fact.mean && fact.mean < fact.ci95_high);
}

/// 旧评分数值文件（无 dimension_scores/emotion/fact 新判据字段）反序列化兼容。
#[test]
fn evaluation_variant_serde_backcompat_new_fields() {
    let old = r#"{
        "variant_id":"v1",
        "description":"d",
        "params":{"theta_gap_minutes":10,"max_msgs_per_block":80,"retrieve_top_k":3},
        "fact_score":0.5,"tone_score":null,"failed_count":0,"items":[]
    }"#;
    let parsed: VariantEvaluation = serde_json::from_str(old).unwrap();
    assert!(parsed.dimension_scores.is_none());
    assert!(parsed.emotion_score.is_none());
    assert!(parsed.fact_score_norm.is_none(), "旧产物无长度归一均分");
    assert!(parsed.fact_score_point.is_none(), "旧产物无事实点均分");
    // 空聚合序列化时省略 dimension_scores（保持最小差异）
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("dimension_scores"), "None 聚合应省略: {s}");
    assert!(!s.contains("fact_score_norm"), "None 均分应省略: {s}");
    assert!(!s.contains("fact_score_point"), "None 均分应省略: {s}");
}

// =========================================================
// 事实维多判据（旧 2-gram 覆盖 + 长度归一 + 子句级事实点）
// =========================================================

/// 内容 2-gram 提取：剔除含标点的组合与两侧皆功能字的组合。
#[test]
fn content_bigrams_filters_punct_and_function_pairs() {
    // 任意一侧为标点 → 剔除
    assert!(
        content_bigrams("我，你").is_empty(),
        "含标点的 2-gram 应剔除"
    );
    // 两侧皆为功能字 → 剔除
    assert!(
        content_bigrams("我是").is_empty(),
        "两侧功能字的 2-gram 应剔除"
    );
    // 只有一侧为功能字 → 保留（内容字参与的组合仍计入）
    assert_eq!(content_bigrams("团子是").len(), 2, "内容字参与的组合应保留");
    // 不足 2 字 / 空串 → 无 2-gram
    assert!(content_bigrams("猫").is_empty());
    assert!(content_bigrams("").is_empty());
}

/// 长度归一命中率边界：一致 / 子串 / 无重叠 / 空回复 / 参考过短。
#[test]
fn keyword_hit_norm_score_extremes() {
    let reference = "团子是三花猫";
    assert!(
        (keyword_hit_norm_score(reference, reference) - 1.0).abs() < 1e-9,
        "回复与参考一致应满分"
    );
    assert!(
        (keyword_hit_norm_score("团子", reference) - 1.0).abs() < 1e-9,
        "回复为参考子串时分母取 min(参考, 回复) → 满分"
    );
    assert_eq!(keyword_hit_norm_score("唔唔唔唔", reference), 0.0, "无重叠");
    assert_eq!(keyword_hit_norm_score("", reference), 0.0, "空回复");
    assert_eq!(keyword_hit_norm_score("   ", reference), 0.0, "空白回复");
    assert_eq!(
        keyword_hit_norm_score("猫", "猫"),
        0.0,
        "参考无内容 2-gram（过短）"
    );
}

/// 长度中性：短回复在旧口径（分母固定为参考 2-gram 总数）下被机械压低，新口径不再衰减。
#[test]
fn keyword_hit_norm_is_length_neutral_for_short_reply() {
    let reference = "用户去年收养了一只猫，取名团子";
    let reply = "团子";

    // 复算旧口径（分母 = 参考全部 2-gram，含标点组合；分子 = 回复命中的参考 2-gram 数）。
    let ref_chars: Vec<char> = reference.chars().collect();
    let ref_all: std::collections::HashSet<(char, char)> =
        ref_chars.windows(2).map(|w| (w[0], w[1])).collect();
    let reply_chars: Vec<char> = reply.chars().collect();
    let hit = reply_chars
        .windows(2)
        .filter(|w| ref_all.contains(&(w[0], w[1])))
        .count();
    assert_eq!(ref_all.len(), 14, "旧口径分母 = 参考 2-gram 总数");
    assert_eq!(hit, 1, "旧口径分子 = 回复命中的参考 2-gram 数");
    let old_score = hit as f64 / ref_all.len() as f64;
    assert!(old_score < 0.1, "旧口径下短回复被压到 {old_score}");

    // 新口径：命中数 / min(参考内容 2-gram 数, 回复内容 2-gram 数) = 3 / min(12, 3) → 1.0。
    let norm = keyword_hit_norm_score(reply, reference);
    assert!((norm - 1.0).abs() < 1e-9, "长度归一口径应满分，实际 {norm}");
    assert!(norm > old_score, "短回复在长度归一口径下不被压低");
}

/// 事实点召回：按子句计分，部分体现 / 全覆盖 / 空回复 / 参考无有效子句。
#[test]
fn fact_point_score_counts_hit_clauses() {
    let reference = "他养了猫，他喜欢狗。他怕蛇";
    let one = fact_point_score("他养了猫", reference);
    assert!(
        (one - 1.0 / 3.0).abs() < 1e-9,
        "3 个子句命中 1 个 → 1/3，实际 {one}"
    );
    assert!(
        (fact_point_score(reference, reference) - 1.0).abs() < 1e-9,
        "回复与参考一致应全命中"
    );
    assert_eq!(fact_point_score("", reference), 0.0, "空回复");
    assert_eq!(fact_point_score("他养了猫", "，，"), 0.0, "参考无有效子句");
}

/// 参考子句切分：中英文标点均切分，丢弃 <2 字片段。
#[test]
fn reference_clauses_split_and_drop_short_segments() {
    assert_eq!(reference_clauses("他养了猫，他喜欢狗。他怕蛇").len(), 3);
    assert_eq!(reference_clauses("他养了猫,他喜欢狗.他怕蛇").len(), 3);
    assert_eq!(
        reference_clauses("猫，狗，他养了三花猫"),
        vec!["他养了三花猫".to_string()],
        "单字片段应丢弃"
    );
    assert_eq!(
        reference_clauses("团子是三花猫"),
        vec!["团子是三花猫".to_string()],
        "无标点时整体为单子句"
    );
}

/// 事实维子评分向后兼容：旧产物无新判据字段 → None；新产物可解析到 Some。
#[test]
fn fact_item_score_serde_backcompat_new_criteria() {
    let old = r#"{"cosine":0.7,"keyword_hit":0.3,"score":0.54}"#;
    let parsed: FactItemScore = serde_json::from_str(old).expect("旧事实维子评分应可反序列化");
    assert_eq!(parsed.cosine, Some(0.7));
    assert!(parsed.keyword_hit_norm.is_none(), "旧产物无长度归一判据");
    assert!(parsed.fact_point.is_none(), "旧产物无事实点判据");
    assert!(parsed.score_norm.is_none(), "旧产物无长度归一综合分");
    assert!(parsed.score_point.is_none(), "旧产物无事实点综合分");

    let new = r#"{"cosine":0.7,"keyword_hit":0.3,"score":0.54,
        "keyword_hit_norm":1.0,"fact_point":0.3333333333333333,
        "score_norm":0.82,"score_point":0.5533333333333333}"#;
    let parsed: FactItemScore = serde_json::from_str(new).expect("新产物应可反序列化");
    assert_eq!(parsed.keyword_hit_norm, Some(1.0));
    let point = parsed.fact_point.expect("新产物应含事实点判据");
    assert!((point - 1.0 / 3.0).abs() < 1e-12);
    let norm = parsed.score_norm.expect("新产物应含长度归一综合分");
    assert!((norm - 0.82).abs() < 1e-12);
    let scored = parsed.score_point.expect("新产物应含事实点综合分");
    assert!((scored - 0.5533333333333333).abs() < 1e-12);
}

/// 逐轮聚合纳入两个新事实维判据：无 embedder 降级时与纯函数逐轮均值一致。
#[tokio::test]
async fn aggregate_round_scores_includes_norm_and_point_dimensions() {
    let reference = "他养了猫，他喜欢狗。他怕蛇";
    let reply = "他养了猫";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let rounds: Vec<ProbeVariantResult> =
        vec![fact_round(reply), fact_round(reply), fact_round(reply)];
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden), 0).await;
    let dims: Vec<&str> = agg.iter().map(|d| d.dimension.as_str()).collect();
    assert_eq!(
        dims,
        vec!["fact", "fact_norm", "fact_point"],
        "事实维三口径各一条聚合"
    );

    let norm = agg
        .iter()
        .find(|d| d.dimension == "fact_norm")
        .expect("应有 fact_norm 聚合");
    let point = agg
        .iter()
        .find(|d| d.dimension == "fact_point")
        .expect("应有 fact_point 聚合");
    // 降级（无 embedding）时综合分 = 纯关键词项，故与纯函数值一致
    let expect_norm = keyword_hit_norm_score(reply, reference);
    let expect_point = fact_point_score(reply, reference);
    assert!(
        (norm.mean - expect_norm).abs() < 1e-9,
        "长度归一均值应与纯函数一致：{} vs {expect_norm}",
        norm.mean
    );
    assert!(
        (point.mean - expect_point).abs() < 1e-9,
        "事实点均值应与纯函数一致：{} vs {expect_point}",
        point.mean
    );
    assert!(
        (point.mean - 1.0 / 3.0).abs() < 1e-9,
        "3 个子句命中 1 个 → 1/3，实际 {}",
        point.mean
    );
    assert_eq!(norm.n, 3);
    assert_eq!(point.n, 3);

    // 旧口径（分母 = 参考 2-gram 总数 12，命中 3）冻结为 0.25；
    // 新长度归一口径为 1.0：短回复不再被机械压低。
    let old = agg
        .iter()
        .find(|d| d.dimension == "fact")
        .expect("应有 fact 聚合");
    assert!(
        (old.mean - 0.25).abs() < 1e-9,
        "旧 2-gram 覆盖口径应冻结为 0.25，实际 {}",
        old.mean
    );
    assert!(norm.mean > old.mean, "长度归一口径应高于旧口径");
}

/// cosine 可用时：新综合分与旧综合分同权重（0.6 / 0.4），仅关键词项不同。
#[tokio::test]
async fn fact_norm_and_point_scores_reuse_old_weights() {
    let reference = "他养了猫，他喜欢狗。他怕蛇";
    let reply = "他养了猫";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let embedder: Option<Arc<dyn EmbeddingProvider>> = Some(Arc::new(TableEmbedding {
        table: vec![(reply, vec![1.0, 0.0]), (reference, vec![1.0, 1.0])],
    }));
    let rounds: Vec<ProbeVariantResult> = vec![fact_round(reply), fact_round(reply)];
    let agg = aggregate_round_dimension_scores(&rounds, &embedder, None, Some(&golden), 0).await;

    // 语义余弦 = [1,0] 与 [1,1] 的夹角余弦 = 1/√2
    let cos = 1.0 / 2.0f64.sqrt();
    let kw_norm = keyword_hit_norm_score(reply, reference);
    let point = fact_point_score(reply, reference);
    // 权重与事实维综合分一致：cosine 0.6 + 关键词项 0.4
    let expect_old = 0.6 * cos + 0.4 * 0.25;
    let expect_norm = 0.6 * cos + 0.4 * kw_norm;
    let expect_point = 0.6 * cos + 0.4 * point;

    let get = |name: &str| {
        agg.iter()
            .find(|d| d.dimension == name)
            .unwrap_or_else(|| panic!("应有 {name} 聚合"))
            .mean
    };
    let old = get("fact");
    assert!(
        (old - expect_old).abs() < 1e-9,
        "旧综合分 {old} 应为 {expect_old}"
    );
    let norm = get("fact_norm");
    assert!(
        (norm - expect_norm).abs() < 1e-9,
        "长度归一综合分 {norm} 应为 {expect_norm}"
    );
    let scored = get("fact_point");
    assert!(
        (scored - expect_point).abs() < 1e-9,
        "事实点综合分 {scored} 应为 {expect_point}"
    );
    assert!(norm > old && scored > old, "新口径应高于旧口径（短回复）");
}

// =========================================================
// 消融对比报告统计
// =========================================================

/// 固定向量 mock embedding：按文本查表返回预设向量（未命中返回 `[1.0, 0.0]`）。
struct TableEmbedding {
    table: Vec<(&'static str, Vec<f32>)>,
}

#[async_trait::async_trait]
impl EmbeddingProvider for TableEmbedding {
    async fn embed(&self, text: &str) -> ramaria_core::error::RamariaResult<Vec<f32>> {
        Ok(self
            .table
            .iter()
            .find(|(k, _)| *k == text)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| vec![1.0, 0.0]))
    }

    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> ramaria_core::error::RamariaResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        EmbeddingModelInfo {
            model_id: "table-embedding".to_string(),
            dimension: 2,
        }
    }

    async fn validate(&self) -> ramaria_core::error::RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> ramaria_core::error::RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}
