//! crates/ramaria-cli/src/commands/probe/tests.rs - probe 命令单元测试
//!
//! 设计特点:
//! - 覆盖数据集构建（build / fixture 兜底 / 文件解析 / 序列化往返）与确定性抽样复现性。
//! - 覆盖档位与消融 Profile（默认代表配对、F0~F4 / S_* / B0~B1 注入闸门映射）。
//! - 覆盖自动评分（fact / tone / emotion 判定）、--repeat 逐轮聚合与旧格式向后兼容。
//! - 覆盖对比报告统计（Wilcoxon / Cohen's d / BH-FDR / normal-CDF）与消融显著性归因。
//! - 覆盖缺失/非法输入文件统一归为业务校验失败（RamariaError::Validation）。

use super::*;
use ramaria_core::types::PersonaKind;

// 以下为子模块中仅测试使用的内部函数/类型与统一错误类型，
// 迁移后在此显式引入（根文件仅保留运行时 `use`，非测试编译零多余引用）。
use super::evaluate::{
    FactItemScore, ItemEvaluation, ProbeEvaluation, VariantEvaluation,
    aggregate_round_dimension_scores, load_golden_references, read_experiment, score_emotion_item,
};
use super::report::{
    bh_fdr_adjust, build_ablation_report, cohens_d_paired, erf_approx, normal_cdf,
    read_manual_scores, wilcoxon_signed_rank_p,
};
use super::run::{aggregate_repeat_stats, filter_variants, metric_stat, t_critical_975};
use super::types::DATASET_SCHEMA_VERSION;
use ramaria_core::error::RamariaError;
use std::path::Path;

// ---- DeterministicRng ----

#[test]
fn rng_same_seed_same_sequence() {
    let mut a = DeterministicRng::new(42);
    let mut b = DeterministicRng::new(42);
    for _ in 0..100 {
        assert_eq!(a.next_u64(), b.next_u64(), "同 seed 序列必须一致");
    }
}

#[test]
fn rng_different_seed_different_sequence() {
    let mut a = DeterministicRng::new(1);
    let mut b = DeterministicRng::new(2);
    let mut same = 0;
    for _ in 0..10 {
        if a.next_u64() == b.next_u64() {
            same += 1;
        }
    }
    assert!(same <= 1, "不同 seed 的序列应几乎完全不同（同次数={same}）");
}

#[test]
fn rng_shuffle_is_permutation() {
    let mut rng = DeterministicRng::new(7);
    let mut items = vec![1, 2, 3, 4, 5];
    rng.shuffle(&mut items);
    let mut sorted = items.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![1, 2, 3, 4, 5], "洗牌必须是排列（不增不减）");
}

// ---- sample_with_fallback ----

#[test]
fn sample_fallback_uses_fixture_when_no_candidates() {
    let (items, real) = sample_with_fallback::<i32>(&[], &[10, 20, 30], 2, 99);
    assert_eq!(items, vec![10, 20]);
    assert_eq!(real, 0, "无真实候选时 real=0");
}

#[test]
fn sample_fallback_deterministic_same_seed() {
    let cands = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let (a, _) = sample_with_fallback(&cands, &[0], 4, 123);
    let (b, _) = sample_with_fallback(&cands, &[0], 4, 123);
    assert_eq!(a, b, "同 seed 抽样结果必须一致（可复跑）");
    assert_eq!(a.len(), 4);
}

#[test]
fn sample_fallback_pads_with_fixture_when_short() {
    let cands = vec![1, 2];
    let fixture = vec![100, 200, 300];
    let (items, real) = sample_with_fallback(&cands, &fixture, 4, 5);
    assert_eq!(real, 2);
    assert_eq!(items.len(), 4, "不足部分必须用夹具补满");
    // 真实数据在前（洗牌后顺序不定，按集合比较）
    let mut head = items[..2].to_vec();
    head.sort_unstable();
    assert_eq!(head, vec![1, 2], "真实数据应排在前面");
    assert_eq!(&items[2..], &[100, 200], "夹具补齐排在真实数据之后");
}

// ---- 档位 ----

#[test]
fn default_variants_are_representative_pairs() {
    let variants = default_variants();
    assert_eq!(variants.len(), 4);
    // baseline 即对照基准值
    let base = &variants[0];
    assert_eq!(base.id, "baseline");
    assert_eq!(base.theta_gap_minutes, 10);
    assert_eq!(base.max_msgs_per_block, 80);
    assert_eq!(base.retrieve_top_k, 3);
    // 每个档位只动一个参数（相对定稿基准 baseline）
    for v in &variants[1..] {
        let changed = [
            v.theta_gap_minutes != base.theta_gap_minutes,
            v.max_msgs_per_block != base.max_msgs_per_block,
            v.retrieve_top_k != base.retrieve_top_k,
        ]
        .iter()
        .filter(|b| **b)
        .count();
        assert_eq!(changed, 1, "档位 {} 应只变化一个参数", v.id);
    }
}

// ---- read_experiment（实验结果文件缺失/非法 → 业务校验失败）----

#[test]
fn read_experiment_missing_file_is_validation_error() {
    let err = read_experiment(Path::new("/nonexistent/run.json")).expect_err("文件缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(
        matches!(ramaria_err, Some(RamariaError::Validation { .. })),
        "文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
    );
}

#[test]
fn read_experiment_malformed_json_is_validation_error() {
    let path = std::env::temp_dir().join("ramaria_probe_bad_experiment.json");
    std::fs::write(&path, "{ not json").expect("写入临时文件失败");
    let err = read_experiment(&path).expect_err("非法 JSON 必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(matches!(ramaria_err, Some(RamariaError::Validation { .. })));
    let _ = std::fs::remove_file(&path);
}

// ---- 统计法（--repeat）----

/// metric_stat：单样本退化为该值，stddev=0，CI=该值。
#[test]
fn metric_stat_single_sample_degenerates() {
    let s = metric_stat(&[42.0]);
    assert_eq!(s.n, 1);
    assert_eq!(s.mean, 42.0);
    assert_eq!(s.stddev, 0.0);
    assert_eq!(s.ci_low, 42.0);
    assert_eq!(s.ci_high, 42.0);
}

/// metric_stat：空样本 → 全零。
#[test]
fn metric_stat_empty_is_zero() {
    let s = metric_stat(&[]);
    assert_eq!(s.n, 0);
    assert_eq!(s.mean, 0.0);
    assert_eq!(s.stddev, 0.0);
    assert_eq!(s.ci_low, 0.0);
    assert_eq!(s.ci_high, 0.0);
}

/// metric_stat：多样本 → 均值正确、stddev 为样本标准差、CI 对称且随 n 增大收窄。
#[test]
fn metric_stat_multiple_mean_stddev_ci() {
    let samples = [10.0, 12.0, 11.0]; // mean=11
    let s = metric_stat(&samples);
    assert_eq!(s.n, 3);
    assert!((s.mean - 11.0).abs() < 1e-9, "均值应为 11, 实际 {}", s.mean);
    // 样本标准差 = sqrt(((1)^2+(-1)^2+0)/2) = sqrt(1)=1
    assert!(
        (s.stddev - 1.0).abs() < 1e-9,
        "stddev 应为 1, 实际 {}",
        s.stddev
    );
    // t(2,0.975)=4.303, half = 4.303*1/sqrt(3)
    let half = 4.303 / 3.0f64.sqrt();
    assert!((s.ci_low - (11.0 - half)).abs() < 1e-6);
    assert!((s.ci_high - (11.0 + half)).abs() < 1e-6);
    assert!(s.ci_low < s.mean && s.mean < s.ci_high);
}

/// metric_stat：n 增大 → 置信区间收窄（同一分布更稳）。
#[test]
fn metric_stat_more_samples_narrower_ci() {
    let small = metric_stat(&[10.0, 12.0, 11.0, 10.5, 11.2]);
    let bigger = metric_stat(&[
        10.0, 12.0, 11.0, 10.5, 11.2, 10.8, 11.4, 10.9, 11.1, 10.7, 11.3, 10.6, 11.0, 11.2, 10.9,
        11.1, 10.8, 11.0, 10.9, 11.1, 11.0, 11.0, 11.0, 11.0,
    ]);
    let w_small = small.ci_high - small.ci_low;
    let w_bigger = bigger.ci_high - bigger.ci_low;
    assert!(
        w_bigger < w_small,
        "样本量增大后 CI 应收窄, 小 {w_small} vs 大 {w_bigger}"
    );
}

/// t_critical_975：边界值正确且单调递减趋近于 2。
#[test]
fn t_critical_975_table_and_approximation() {
    assert!((t_critical_975(2) - 12.706).abs() < 1e-6);
    assert!((t_critical_975(5) - 2.776).abs() < 1e-6);
    // 超表项 → 近似 2.0
    assert_eq!(t_critical_975(100), 2.0);
    // 单调递减（自由度越高，临界值越小）
    assert!(t_critical_975(3) < t_critical_975(2));
    assert!(t_critical_975(8) < t_critical_975(5));
}

/// aggregate_repeat_stats：按档位+item 配对，缺轮样本以实际计数。
#[test]
fn aggregate_repeat_stats_pairs_by_variant_and_item() {
    // 构造两个 round 的 ProbeExperiment
    fn round(item_chars: &[(usize, usize)]) -> ProbeExperiment {
        let vr = ProbeVariantResult {
            variant_id: "v1".to_string(),
            description: "档位".to_string(),
            params: VariantParams {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
                retrieve_top_k: 3,
                ablation: None,
            },
            runs: item_chars
                .iter()
                .map(|(id, chars)| ProbeRunItem {
                    item_id: format!("fact-{id:04}"),
                    dimension: "fact".to_string(),
                    question: "q".to_string(),
                    reply: String::new(),
                    metrics: ProbeMetrics {
                        reply_chars: *chars,
                        elapsed_ms: 100,
                    },
                    error: None,
                })
                .collect(),
            failed_count: 0,
        };
        ProbeExperiment {
            dataset_file: "d".to_string(),
            dataset_seed: 1,
            persona_uid: "p".to_string(),
            rebuild_utt: true,
            variants: vec![vr],
            repeat: None,
            generated_at: "t".to_string(),
        }
    }
    let r1 = round(&[(1, 10), (2, 20)]);
    let r2 = round(&[(1, 14), (2, 24)]);
    let stats = aggregate_repeat_stats(&[r1, r2]);
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].per_item.len(), 2);
    // item1: chars=[10,14] mean=12
    let it1 = &stats[0].per_item[0];
    assert_eq!(it1.item_id, "fact-0001");
    assert!((it1.reply_chars.mean - 12.0).abs() < 1e-6);
    assert_eq!(it1.reply_chars.n, 2);
    // item2: chars=[20,24] mean=22
    let it2 = &stats[0].per_item[1];
    assert!((it2.reply_chars.mean - 22.0).abs() < 1e-6);
    // 缺口 A：rounds 保留该档位每一轮的完整结果明细（逐轮全量 reply）
    assert_eq!(stats[0].rounds.len(), 2, "应保留两轮的完整结果");
    // round1 item chars=10 / round2 item chars=14
    assert_eq!(stats[0].rounds[0].runs[0].metrics.reply_chars, 10);
    assert_eq!(stats[0].rounds[1].runs[0].metrics.reply_chars, 14);
    assert_eq!(stats[0].rounds[0].runs.len(), 2);
    assert_eq!(stats[0].rounds[1].runs.len(), 2);
}

/// 缺口 A 向后兼容：旧 repeat 聚合 JSON 无 `rounds` 字段时反序列化为空，
/// 序列化时空 `rounds` 被省略（不破坏旧文件读/写与契约）。
#[test]
fn repeat_rounds_serde_roundtrip_and_backcompat() {
    // 新格式：rounds 非空，序列化应保留逐轮明细。
    let with_rounds = VariantRepeatStats {
        variant_id: "v1".to_string(),
        per_item: vec![],
        rounds: vec![ProbeVariantResult {
            variant_id: "v1".to_string(),
            description: "d".to_string(),
            params: VariantParams {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
                retrieve_top_k: 3,
                ablation: None,
            },
            runs: vec![],
            failed_count: 0,
        }],
    };
    let roundtrip: VariantRepeatStats =
        serde_json::from_str(&serde_json::to_string(&with_rounds).unwrap()).unwrap();
    assert_eq!(roundtrip.rounds.len(), 1);

    // 旧格式：JSON 无 rounds 字段 → 反序列化 rounds 为空（serde default）。
    let old = r#"{"variant_id":"v1","per_item":[]}"#;
    let parsed: VariantRepeatStats = serde_json::from_str(old).unwrap();
    assert!(parsed.rounds.is_empty());

    // 空的 rounds 序列化时应省略该键（skip_serializing_if），保持与旧文件最小差异。
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("rounds"), "空 rounds 应省略，实际: {s}");
}

// ---- 输入文件缺失统一归业务校验失败（--results / --evaluation / --calibration / --dataset / --source）----

#[test]
fn read_manual_scores_missing_file_is_validation_error() {
    let err =
        read_manual_scores(Path::new("/nonexistent/calib.json")).expect_err("校准文件缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(
        matches!(ramaria_err, Some(RamariaError::Validation { .. })),
        "校准文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
    );
}

#[test]
fn load_golden_references_missing_file_is_validation_error() {
    let err = load_golden_references(Path::new("/nonexistent/dataset.json"))
        .expect_err("数据集缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(matches!(ramaria_err, Some(RamariaError::Validation { .. })));
}

// ---- fixture ----

#[test]
fn fixture_data_covers_default_scale() {
    assert!(fixture_tone_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
    assert!(fixture_fact_events().len() >= DEFAULT_QUESTIONS_PER_DIM);
    assert!(fixture_emotion_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
    // emotion 夹具的 question 必须命中情感线索（否则不会被收集/评分语义判定）
    for (q, _) in fixture_emotion_pairs() {
        assert!(has_emotion_cue(&q), "emotion 夹具问题应含情感线索: {q}");
    }
}

// ---- select_target_persona（不按发言量，白名单过滤对方）----

/// 构造测试 persona。
fn test_persona(uid: &str, kind: PersonaKind) -> ramaria_core::types::Persona {
    ramaria_core::types::Persona::new(
        uid.to_string(),
        uid.to_string(),
        kind,
        1,
        "test".to_string(),
    )
}

#[test]
fn select_persona_excludes_user_kind() {
    // 我方（kind=user）不得入选探针目标
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("char-0001", PersonaKind::Char),
    ];
    assert_eq!(select_target_persona(&personas, None), "char-0001");
}

#[test]
fn select_persona_first_whitelisted() {
    // 多个对方 persona：取第一个白名单，不引入发言量排序
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("anim-0001", PersonaKind::Anim),
        test_persona("char-0001", PersonaKind::Char),
        test_persona("hist-0001", PersonaKind::Hist),
    ];
    assert_eq!(select_target_persona(&personas, None), "anim-0001");
}

#[test]
fn select_persona_explicit_wins() {
    // 显式 --persona 优先（不校验 kind，尊重用户指定）
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("char-0001", PersonaKind::Char),
    ];
    assert_eq!(
        select_target_persona(&personas, Some("rama-0001")),
        "rama-0001"
    );
}

#[test]
fn select_persona_all_user_role_falls_back() {
    // 全 user-role 退化场景：无白名单 persona → 默认 char-0001（夹具兜底）
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("user-0002", PersonaKind::User),
    ];
    assert_eq!(select_target_persona(&personas, None), DEFAULT_PERSONA);
}

#[test]
fn select_persona_empty_falls_back() {
    assert_eq!(select_target_persona(&[], None), DEFAULT_PERSONA);
}

// ---- build_from_fixture ----

#[test]
fn build_from_fixture_shape() {
    let ds = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
    assert_eq!(ds.schema_version, DATASET_SCHEMA_VERSION);
    assert_eq!(ds.persona_uid, DEFAULT_PERSONA);
    assert_eq!(ds.source, "fixture");
    assert_eq!(ds.dimensions, vec!["tone", "fact", "emotion"]);
    assert_eq!(ds.items.len(), DEFAULT_QUESTIONS_PER_DIM * 3);
    assert_eq!(ds.variants.len(), 4);
    // 全部来自夹具
    assert!(ds.items.iter().all(|i| i.source == "fixture"));
    // 每维恰好 qpd 题
    for dim in ["tone", "fact", "emotion"] {
        assert_eq!(
            ds.items.iter().filter(|i| i.dimension == dim).count(),
            DEFAULT_QUESTIONS_PER_DIM,
            "维度 {dim} 应有 qpd 题"
        );
    }
    // 每题都有 reference 与 id（前缀含 emotion-）
    for item in &ds.items {
        assert!(item.reference.is_some(), "{} 应有参考回答", item.id);
        assert!(
            item.id.starts_with("tone-")
                || item.id.starts_with("fact-")
                || item.id.starts_with("emotion-")
        );
    }
    // seed 固定 → 复跑一致
    let again = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
    let qs: Vec<&str> = ds.items.iter().map(|i| i.question.as_str()).collect();
    let qs2: Vec<&str> = again.items.iter().map(|i| i.question.as_str()).collect();
    assert_eq!(qs, qs2, "同 seed 复跑必须产生相同测试集");
}

// ---- 数据集序列化 roundtrip ----

#[test]
fn dataset_roundtrip_json() {
    let ds = build_from_fixture(DEFAULT_PERSONA, 3, 42);
    let json = serde_json::to_string(&ds).expect("序列化失败");
    let back: ProbeDataset = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(back.items.len(), ds.items.len());
    assert_eq!(back.items[0].question, ds.items[0].question);
    assert_eq!(back.variants.len(), ds.variants.len());
}

// ---- 问题模板 ----

#[test]
fn fact_question_template() {
    let (q, _ref, title) = fixture_fact_events()[0].clone();
    assert!(q.contains(&title), "事实记忆问题应包含事件标题");
    assert!(q.contains("还记得"), "事实记忆问题应使用回忆问法");
}

// ---- filter_variants ----

#[test]
fn filter_variants_selects_and_ignores_unknown() {
    let variants = default_variants();
    let filtered = filter_variants(&variants, Some("baseline,top_k_1,nonexistent"));
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].id, "baseline");
    assert_eq!(filtered[1].id, "top_k_1");
}

#[test]
fn filter_variants_empty_falls_back_to_all() {
    let variants = default_variants();
    let filtered = filter_variants(&variants, Some("bad,bad2"));
    assert_eq!(filtered.len(), 4, "过滤为空时应回退全部档位");
}

// ---- 数据源文件解析 ----

#[test]
fn build_from_file_parses_source_json() {
    let tmp = std::env::temp_dir().join(format!("ramaria_probe_src_{}", std::process::id()));
    let path = tmp.join("source.json");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        &path,
        r#"{
            "persona_uid": "char-0009",
            "messages": [
                {"question": "今天好累", "reply": "早点休息"},
                {"question": "周末去哪", "reply": "去公园"}
            ],
            "events": [
                {"title": "学钢琴", "summary": "今年开始学钢琴，会弹《致爱丽丝》"}
            ]
        }"#,
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let ds = rt
        .block_on(build_from_file(&path, DEFAULT_PERSONA, 3, 7))
        .expect("文件构建应成功");
    let _ = std::fs::remove_dir_all(&tmp);

    assert_eq!(ds.persona_uid, "char-0009", "文件中的 persona_uid 应优先");
    assert_eq!(ds.source, "file");
    assert_eq!(ds.items.len(), 9, "3 维 × 3 题");
    // 真实数据在前：tone 2 条 + fact 1 条 + emotion 1 条
    // （"今天好累"含情感线索"累" → 同时进入 emotion 候选；"周末去哪"不含）。
    assert_eq!(ds.items.iter().filter(|i| i.source == "file").count(), 4);
    assert_eq!(ds.items.iter().filter(|i| i.source == "fixture").count(), 5);
    assert_eq!(
        ds.items.iter().filter(|i| i.dimension == "emotion").count(),
        3,
        "emotion 维应补齐 3 题"
    );
    assert_eq!(ds.dimensions, vec!["tone", "fact", "emotion"]);
}

#[test]
fn build_from_file_missing_is_err() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let missing = std::env::temp_dir().join("ramaria_probe_nonexistent.json");
    let result = rt.block_on(build_from_file(&missing, DEFAULT_PERSONA, 3, 7));
    assert!(result.is_err(), "文件不存在应返回 Err（由上层夹具兜底）");
}

// =========================================================
// M5a 消融档位 Profile（D-V17-015 / 技术报告 §16.3）
// =========================================================

/// 全部 11 个名称可解析且往返一致；未知名称返回 None。
#[test]
fn ablation_profile_parse_roundtrip_all_names() {
    assert_eq!(ABLATION_PROFILE_NAMES.len(), 11);
    for name in ABLATION_PROFILE_NAMES {
        let p = AblationProfile::parse_name(name).unwrap_or_else(|| panic!("名称 {name} 应可解析"));
        assert_eq!(p.name(), name, "解析后名称往返一致");
    }
    assert!(AblationProfile::parse_name("unknown").is_none());
    assert!(AblationProfile::parse_name("").is_none());
    assert!(AblationProfile::parse_name("f0").is_none(), "大小写敏感");
}

/// ablation_variants：11 档、id=Profile 名、utt 取定稿基准、ablation 回显。
#[test]
fn ablation_variants_shape_and_baseline_utt() {
    let variants = ablation_variants();
    assert_eq!(variants.len(), 11);
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
    let mut cfg = ramaria_core::config::RamariaConfig::default();
    cfg.injection = ramaria_core::config::InjectionGate::all_off();
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

/// S_* 前置单层：B1 基座（memory_rag）之上只开目标层。
#[test]
fn ablation_profile_s_group_gates() {
    let mut sb = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SBehavior.apply_to(&mut sb);
    assert!(sb.injection.memory_rag && sb.injection.behavior);
    assert!(!sb.injection.knowledge && !sb.injection.speaking_style);
    assert!(!sb.injection.examples && !sb.injection.utt);
    assert!(!sb.injection.narrative && !sb.injection.bridge);

    let mut sk = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SKnowledge.apply_to(&mut sk);
    assert!(sk.injection.memory_rag && sk.injection.knowledge);
    assert!(!sk.injection.behavior);

    let mut se = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SExpression.apply_to(&mut se);
    assert!(se.injection.memory_rag);
    assert!(se.injection.speaking_style && se.injection.examples && se.injection.utt);
    assert!(!se.injection.behavior && !se.injection.knowledge);
    assert!(!se.injection.narrative && !se.injection.bridge);

    let mut sn = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SNarrative.apply_to(&mut sn);
    assert!(sn.injection.memory_rag);
    assert!(sn.injection.narrative && sn.injection.bridge);
    assert!(!sn.injection.utt && !sn.injection.behavior && !sn.injection.knowledge);
    assert!(!sn.injection.speaking_style && !sn.injection.examples);
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
    };
    let json = serde_json::to_string(&v).unwrap();
    assert!(
        json.contains("\"ablation\":\"F1\""),
        "ablation 应序列化: {json}"
    );
    let back: ProbeVariant = serde_json::from_str(&json).unwrap();
    assert_eq!(back.ablation.as_deref(), Some("F1"));

    // ablation=None 序列化时省略该键（保持 M1 旧产物最小差异）。
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
// M5a emotion 第三维（T-V17-5a-002）
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
// M5a --repeat 逐轮评分聚合（T-V17-5a-003）
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
    let agg = aggregate_round_dimension_scores(&[], &None, None, None).await;
    assert!(agg.is_empty());
}

/// 三轮回复与 golden 完全一致 → fact 轮均分恒 1.0，n=3、std=0、CI 退化。
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
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden)).await;
    assert_eq!(agg.len(), 1, "只有 fact 维聚合");
    assert_eq!(agg[0].dimension, "fact");
    assert_eq!(agg[0].n, 3, "有效轮数 = 3");
    assert!((agg[0].mean - 1.0).abs() < 1e-9, "满分均值应为 1.0");
    assert_eq!(agg[0].std, 0.0);
    assert!((agg[0].ci95_low - 1.0).abs() < 1e-9);
    assert!((agg[0].ci95_high - 1.0).abs() < 1e-9);
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
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden)).await;
    assert_eq!(agg[0].n, 3);
    assert!(
        agg[0].mean > 0.0 && agg[0].mean < 1.0,
        "波动后均值应介于 0..1"
    );
    assert!(agg[0].std > 0.0, "质量波动应产生正 std");
    assert!(agg[0].ci95_low < agg[0].mean && agg[0].mean < agg[0].ci95_high);
}

/// 旧评分数值文件（无 dimension_scores/emotion 字段）反序列化兼容。
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
    // 空聚合序列化时省略 dimension_scores（保持最小差异）
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("dimension_scores"), "None 聚合应省略: {s}");
}

// =========================================================
// M5a 消融对比报告统计（T-V17-5a-004）
// =========================================================

/// erf / 正态 CDF 关键值：cdf(0)=0.5，cdf(1.96)≈0.975。
#[test]
fn normal_cdf_key_values() {
    assert!((normal_cdf(0.0) - 0.5).abs() < 1e-9);
    assert!((normal_cdf(1.96) - 0.975).abs() < 0.005);
    assert!((normal_cdf(-1.96) - 0.025).abs() < 0.005);
    assert!((erf_approx(0.0)).abs() < 1e-9);
}

/// Wilcoxon：单向强效应 → p 小；符号混合 → p 大（接近 1 侧）。
#[test]
fn wilcoxon_signed_rank_directionality() {
    // 8 个全正差分（不同绝对值避免全结）→ 秩和显著偏离零
    let diffs: Vec<f64> = (1..=8).map(|i| i as f64 * 0.1).collect();
    let p_strong = wilcoxon_signed_rank_p(&diffs).expect("n≥5 应可检验");
    assert!(p_strong < 0.05, "单向效应 p 应小，实际 {p_strong}");
    // 正负各半抵消 → p 大
    let mixed = vec![0.2, -0.3, 0.4, -0.5, 0.6, -0.7];
    let p_mixed = wilcoxon_signed_rank_p(&mixed).expect("n≥5 应可检验");
    assert!(p_mixed > 0.1, "符号混合 p 应大，实际 {p_mixed}");
    // 样本过小（n<5）→ None
    assert!(wilcoxon_signed_rank_p(&[0.1, 0.2, 0.3]).is_none());
}

/// Cohen's d：零方差非零均值 → ±10 标记；零均值 → 0。
#[test]
fn cohens_d_edge_cases() {
    assert_eq!(cohens_d_paired(&[1.0, 1.0, 1.0, 1.0]), 10.0);
    assert_eq!(cohens_d_paired(&[-0.5, -0.5]), -10.0);
    assert_eq!(cohens_d_paired(&[1.0, -1.0]), 0.0);
    assert!((cohens_d_paired(&[1.0, 2.0]) - 2.121).abs() < 0.01);
    assert_eq!(cohens_d_paired(&[]), 0.0);
}

/// BH FDR：单调校正且首尾正确。
#[test]
fn bh_fdr_adjust_monotonic() {
    let p = vec![0.01, 0.04, 0.2];
    let q = bh_fdr_adjust(&p);
    // 预期: [0.03, 0.06, 0.2]
    assert!((q[0] - 0.03).abs() < 1e-12);
    assert!((q[1] - 0.06).abs() < 1e-12);
    assert!((q[2] - 0.2).abs() < 1e-12);
    // 空输入
    assert!(bh_fdr_adjust(&[]).is_empty());
}

/// 构造一个合成评分数值档位（纯 fact 维度，给定逐题分数）。
fn eval_variant_scores(id: &str, scores: &[f64]) -> VariantEvaluation {
    let items = scores
        .iter()
        .enumerate()
        .map(|(i, s)| ItemEvaluation {
            item_id: format!("fact-{:04}", i + 1),
            dimension: "fact".to_string(),
            question: String::new(),
            reference: None,
            reply_preview: String::new(),
            fact: Some(FactItemScore {
                cosine: Some(*s),
                keyword_hit: *s,
                score: *s,
            }),
            tone: None,
            emotion: None,
            error: None,
        })
        .collect();
    VariantEvaluation {
        variant_id: id.to_string(),
        description: format!("{id} 档位"),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: Some(id.to_string()),
        },
        fact_score: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: None,
        failed_count: 0,
        items,
    }
}

/// 集成：F0（高分）vs F1（同题低分）→ F1/fact 行显著且方向 down。
#[test]
fn build_ablation_report_marks_removal_effect() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("F0", &[0.9, 0.9, 0.9, 0.9, 0.9]),
            eval_variant_scores("F1", &[0.5, 0.5, 0.5, 0.5, 0.5]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);
    assert_eq!(report.baseline_variant, "F0");
    let row = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "F1" && r.dimension == "fact")
        .expect("应有 F1/fact 行");
    assert_eq!(row.n_pairs, 5);
    assert!(row.significant, "F1 移除行为层后应显著下降");
    assert_eq!(row.direction, "down");
    assert!(row.mean_diff < 0.0);
    assert!(row.p_fdr < 0.05);
    assert!(row.ci95_high < 0.0, "CI 不含 0");
}

/// S 组：B1（低分基座）vs S_behavior（高分单层）→ up 方向。
#[test]
fn build_ablation_report_s_group_positive() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B1", &[0.4, 0.4, 0.4, 0.4, 0.4]),
            eval_variant_scores("S_behavior", &[0.8, 0.8, 0.8, 0.8, 0.8]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);
    assert_eq!(report.baseline_variant, "B1");
    let row = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "S_behavior" && r.dimension == "fact")
        .expect("应有 S_behavior/fact 行");
    assert!(row.significant, "S_behavior 单层注入应显著正向");
    assert_eq!(row.direction, "up");
    assert!(row.mean_diff > 0.0);
}
