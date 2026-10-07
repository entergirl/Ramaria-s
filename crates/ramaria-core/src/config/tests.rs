//! crates/ramaria-core/src/config/tests.rs - Ramaria 应用配置类型单元测试
//!
//! 设计特点:
//! - 校验默认配置合法性与版本常量
//! - 覆盖各配置组的默认值与覆盖行为
//! - 验证 config.toml 解析与 DB settings 扁平同步
//! - 依赖父模块 re-export 与类型导入，保持断言零改写

use super::*;

use crate::types::{LlmProvider, PersonaKind};

#[test]
fn default_config_is_valid() {
    let cfg = RamariaConfig::default();

    // 版本控制字段
    assert_eq!(cfg.version, CURRENT_APP_VERSION);
    assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);

    // 检索参数默认值校验
    assert_eq!(cfg.retrieval.l0_window_size, 3);
    assert_eq!(cfg.retrieval.rrf_k, 60);
    assert!((cfg.retrieval.similarity_threshold - 0.6).abs() < f64::EPSILON);
    // 摘要路图谱与向量通道默认开启；RAG 格式化参数默认与既有行为等价
    assert!(
        cfg.retrieval.enable_graph,
        "图谱通道默认开启（与既有行为等价）"
    );
    assert!(
        cfg.retrieval.enable_vector,
        "向量通道默认开启（与既有行为等价）"
    );
    assert!(
        cfg.retrieval.enable_keyword_channel,
        "关键词镜像通道默认开启（第四通道）"
    );
    assert!(
        (cfg.retrieval.keyword_weight - 1.0).abs() < f64::EPSILON,
        "关键词镜像通道权重默认 1.0"
    );
    assert_eq!(cfg.retrieval.rag_max_memories, 5);
    assert_eq!(cfg.retrieval.rag_max_summary_chars, 120);
    assert!((cfg.retrieval.rag_share_threshold_user - 0.3).abs() < f64::EPSILON);
    assert!((cfg.retrieval.rag_share_threshold_char - 0.5).abs() < f64::EPSILON);
    assert!((cfg.retrieval.rag_share_threshold_rama - 0.0).abs() < f64::EPSILON);
    assert!(cfg.retrieval.rag_include_graph_entities);

    // 衰减参数
    assert_eq!(cfg.decay.s_l0, 10);
    assert_eq!(cfg.decay.s_l1, 30);
    assert_eq!(cfg.decay.s_l2, 60);
    assert!(cfg.decay.enable_access_boost);

    // Session 参数
    assert_eq!(cfg.session.l1_idle_minutes, 10);
    assert_eq!(cfg.session.max_history_chars, 6000);

    // 阈值
    assert_eq!(cfg.thresholds.l2_trigger_count, 5);
    assert_eq!(cfg.thresholds.l2_trigger_days, 7);

    // 后端默认
    assert_eq!(cfg.backend.provider, LlmProvider::LmStudio);

    // 日志默认
    assert!(!cfg.logging.log_full_prompt);

    // 缓存默认（v1.5）：精确缓存与 L2 指纹默认开启
    assert!(cfg.cache.enabled);
    assert_eq!(cfg.cache.max_entries, 10_000);
    assert_eq!(cfg.cache.eviction, CacheEviction::Lru);
    assert!(cfg.cache.l2_fingerprint_enabled);
    assert!((cfg.cache.l2_similarity_threshold - 0.95).abs() < f64::EPSILON);
    assert_eq!(cfg.cache.l2_recent_events_limit, 200);

    // 行为层默认
    assert!(cfg.behavior.enabled);
    assert!((cfg.behavior.theta_nb - 0.65).abs() < f64::EPSILON);
    assert_eq!(cfg.behavior.min_cluster_size, 3);
    assert!((cfg.behavior.theta_join - 0.7).abs() < f64::EPSILON);
    assert!((cfg.behavior.beta1 - 0.85).abs() < f64::EPSILON);
    assert!((cfg.behavior.beta2 - 0.10).abs() < f64::EPSILON);
    assert!((cfg.behavior.theta_route - 0.6).abs() < f64::EPSILON);
    assert!((cfg.behavior.gamma - 0.7).abs() < f64::EPSILON);
    assert_eq!(cfg.behavior.top_n, 3);
    assert_eq!(cfg.behavior.min_evidence, 5);
    assert_eq!(cfg.behavior.min_n_eff, 5);
    assert!((cfg.behavior.valence_std_limit - 0.5).abs() < f64::EPSILON);
    assert!((cfg.behavior.max_outlier_ratio - 0.6).abs() < f64::EPSILON);
    assert_eq!(cfg.behavior.pending_expire_days, 30);
    assert!((cfg.behavior.evidence_decay_threshold - 0.3).abs() < f64::EPSILON);
    // 权重约束：β1 + β2 ≤ 1（关键词通道 = 1 − β1 − β2 非负）
    assert!(cfg.behavior.beta1 + cfg.behavior.beta2 <= 1.0);

    // 知识层默认：自动抽取默认关闭
    assert!(!cfg.knowledge.auto_fact_detect, "auto_fact_detect 默认关闭");
    assert!(cfg.knowledge.detector_enabled);
    assert!((cfg.knowledge.dedup_cosine_threshold - 0.85).abs() < f64::EPSILON);
    assert_eq!(cfg.knowledge.dedup_keyword_min, 1);
    assert!((cfg.knowledge.corroboration_cosine_threshold - 0.7).abs() < f64::EPSILON);
    assert_eq!(cfg.knowledge.injection_budget_chars, 800);
    assert_eq!(cfg.knowledge.volatile_halflife_days, 30);
    // 三路检索独立字段默认值：知识路新增字段以 0/0.0 表示行为等价占位
    assert_eq!(cfg.knowledge.retrieve_top_k, 0);
    assert!((cfg.knowledge.retrieve_threshold - 0.0).abs() < f64::EPSILON);
    // utt 路 / 摘要路各自生效值（阶段一与既有行为等价）
    assert_eq!(cfg.utt.retrieve_top_k, 3);
    assert_eq!(cfg.retrieval.l1_retrieve_top_k, 4);

    // 画像升级：四开关默认开启
    assert!(cfg.inference.upgrade.cross_version_threshold_085);
    assert!(cfg.inference.upgrade.cold_start_cross_user_prior);
    assert!(cfg.inference.upgrade.drift_restore_real_distribution);
    assert!(cfg.inference.upgrade.causal_latency_emotion_trend);

    // 事件提取降级动态置信度默认开启
    assert!(cfg.event_extraction.degraded_confidence_enabled);
}

/// 三路检索参数互不串扰：单独修改一路，另两路的默认值不受影响。
#[test]
fn three_road_retrieval_params_independent() {
    let base = RamariaConfig::default();

    // 修改 utt 原文路（如调大原文检索条数）不影响摘要路 / 知识路
    let mut utt_tuned = RamariaConfig::default();
    utt_tuned.utt.retrieve_top_k = 9;
    utt_tuned.utt.theta_gap_minutes = 20;
    assert_eq!(
        utt_tuned.retrieval.l1_retrieve_top_k,
        base.retrieval.l1_retrieve_top_k
    );
    assert_eq!(
        utt_tuned.knowledge.retrieve_top_k,
        base.knowledge.retrieve_top_k
    );
    assert!(
        (utt_tuned.knowledge.retrieve_threshold - base.knowledge.retrieve_threshold).abs()
            < f64::EPSILON
    );

    // 修改摘要路（RAG）不影响 utt 路 / 知识路
    let mut rag_tuned = RamariaConfig::default();
    rag_tuned.retrieval.l1_retrieve_top_k = 12;
    rag_tuned.retrieval.similarity_threshold = 0.8;
    rag_tuned.retrieval.rrf_k = 90;
    rag_tuned.retrieval.bm25_weight = 0.5;
    rag_tuned.retrieval.graph_weight = 0.4;
    rag_tuned.retrieval.enable_vector = false;
    rag_tuned.retrieval.rag_max_memories = 8;
    rag_tuned.retrieval.rag_max_summary_chars = 200;
    rag_tuned.retrieval.rag_share_threshold_user = 0.4;
    rag_tuned.retrieval.rag_share_threshold_char = 0.6;
    rag_tuned.retrieval.rag_share_threshold_rama = 0.1;
    rag_tuned.retrieval.rag_include_graph_entities = false;
    assert_eq!(rag_tuned.utt.retrieve_top_k, base.utt.retrieve_top_k);
    assert_eq!(
        rag_tuned.utt.max_msgs_per_block,
        base.utt.max_msgs_per_block
    );
    assert_eq!(
        rag_tuned.knowledge.retrieve_top_k,
        base.knowledge.retrieve_top_k
    );
    assert_eq!(
        rag_tuned.knowledge.retrieve_threshold,
        base.knowledge.retrieve_threshold
    );

    // 修改知识路不影响 utt 路 / 摘要路
    let mut fact_tuned = RamariaConfig::default();
    fact_tuned.knowledge.retrieve_top_k = 7;
    fact_tuned.knowledge.retrieve_threshold = 0.75;
    assert_eq!(fact_tuned.utt.retrieve_top_k, base.utt.retrieve_top_k);
    assert_eq!(
        fact_tuned.retrieval.l1_retrieve_top_k,
        base.retrieval.l1_retrieve_top_k
    );
    assert_eq!(
        fact_tuned.retrieval.enable_vector,
        base.retrieval.enable_vector
    );
    assert_eq!(
        fact_tuned.retrieval.rag_max_memories,
        base.retrieval.rag_max_memories
    );
    assert_eq!(fact_tuned.retrieval.rrf_k, base.retrieval.rrf_k);
}

/// 旧版配置布局（仅含既有键、不含新增知识路字段）仍可解析，
/// 新增字段回退默认值（行为与上一版本等价，旧配置读取兼容）。
#[test]
fn v17_config_layout_still_parses() {
    let old_toml = r#"
version = "1.7.0"

[utt]
enabled = true
theta_gap_minutes = 10
max_msgs_per_block = 80
retrieve_top_k = 3
max_block_chars = 1500

[retrieval]
l0_window_size = 3
l0_retrieve_top_k = 3
l1_retrieve_top_k = 4
l2_retrieve_top_k = 2
similarity_threshold = 0.6
rrf_k = 60
bm25_weight = 1.0
graph_weight = 0.8
retrieval_weight_l2 = 0.8
retrieval_weight_l1 = 1.0
narrative_weighted = true
narrative_top_k = 3

[knowledge]
auto_fact_detect = false
detector_enabled = true
injection_budget_chars = 800
"#;
    let cfg: RamariaConfig = toml::from_str(old_toml).expect("旧版配置布局应可解析");
    assert_eq!(cfg.utt.retrieve_top_k, 3);
    assert_eq!(cfg.retrieval.l1_retrieve_top_k, 4);
    // 新键未配置 → 回退默认（0 / 0.0 = 行为等价占位）
    assert_eq!(cfg.knowledge.retrieve_top_k, 0);
    assert!((cfg.knowledge.retrieve_threshold - 0.0).abs() < f64::EPSILON);
    assert_eq!(cfg.knowledge.injection_budget_chars, 800);
    assert!(cfg.knowledge.detector_enabled);
    // 摘要路新增键未配置 → 回退默认（图谱/向量通道开、关键词镜像通道开、RAG 格式化默认）
    assert!(cfg.retrieval.enable_graph);
    assert!(cfg.retrieval.enable_vector);
    assert!(cfg.retrieval.enable_keyword_channel);
    assert!((cfg.retrieval.keyword_weight - 1.0).abs() < f64::EPSILON);
    assert_eq!(cfg.retrieval.rag_max_memories, 5);
    assert_eq!(cfg.retrieval.rag_max_summary_chars, 120);
    assert!((cfg.retrieval.rag_share_threshold_user - 0.3).abs() < f64::EPSILON);
    assert!(cfg.retrieval.rag_include_graph_entities);
}

#[test]
fn config_serde_roundtrip() {
    let cfg = RamariaConfig::default();
    let json = serde_json::to_string_pretty(&cfg).unwrap();
    let back: RamariaConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(cfg.version, back.version);
    assert_eq!(cfg.schema_version, back.schema_version);
    assert_eq!(cfg.retrieval.rrf_k, back.retrieval.rrf_k);
    assert_eq!(cfg.decay.s_l0, back.decay.s_l0);
    assert_eq!(cfg.backend.provider, back.backend.provider);
    assert!((cfg.decay.salience_multiplier - back.decay.salience_multiplier).abs() < f64::EPSILON);
    // 缓存组 roundtrip
    assert_eq!(cfg.cache.enabled, back.cache.enabled);
    assert_eq!(cfg.cache.max_entries, back.cache.max_entries);
    assert_eq!(cfg.cache.eviction, back.cache.eviction);
    assert_eq!(
        cfg.cache.l2_fingerprint_enabled,
        back.cache.l2_fingerprint_enabled
    );
    // 行为层 roundtrip
    assert_eq!(cfg.behavior.enabled, back.behavior.enabled);
    assert!((cfg.behavior.theta_nb - back.behavior.theta_nb).abs() < f64::EPSILON);
    assert_eq!(
        cfg.behavior.min_cluster_size,
        back.behavior.min_cluster_size
    );
    assert!((cfg.behavior.theta_route - back.behavior.theta_route).abs() < f64::EPSILON);
    assert!((cfg.behavior.gamma - back.behavior.gamma).abs() < f64::EPSILON);
}

#[test]
fn path_config_serde() {
    let paths = PathConfig {
        data_dir: "/tmp/ramaria/data".into(),
        config_dir: "/tmp/ramaria/config".into(),
        log_dir: "/tmp/ramaria/logs".into(),
        vector_index_dir: "/tmp/ramaria/vectors".into(),
    };
    let json = serde_json::to_string(&paths).unwrap();
    let back: PathConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.data_dir, paths.data_dir);
    assert_eq!(back.vector_index_dir, paths.vector_index_dir);
}

#[test]
fn config_json_contains_expected_keys() {
    let cfg = RamariaConfig::default();
    let json = serde_json::to_string(&cfg).unwrap();

    assert!(json.contains("version"));
    assert!(json.contains("schema_version"));
    assert!(json.contains("l0_window_size"));
    assert!(json.contains("rrf_k"));
    assert!(json.contains("s_l0"));
    assert!(json.contains("l1_idle_minutes"));
    assert!(json.contains("l2_trigger_count"));
    assert!(json.contains("bm25_incremental_threshold"));
    assert!(json.contains("log_full_prompt"));
    // v1.4 新增配置组
    assert!(json.contains("theta_gap_minutes"));
    assert!(json.contains("max_msgs_per_block"));
    assert!(json.contains("retrieve_top_k"));
    assert!(json.contains("max_block_chars"));
    assert!(json.contains("persona_kind_whitelist"));
    assert!(json.contains("max_examples"));
    assert!(json.contains("bridge"));
    // 风格统计配置组（表达层 A3）
    assert!(json.contains("style"));
    assert!(json.contains("auto_translate"));
    assert!(json.contains("min_sample_count"));
    assert!(json.contains("relative_boost_ratio"));
    assert!(json.contains("z_critical"));
    // 弱反馈环配置组（H2）
    assert!(json.contains("feedback"));
    assert!(json.contains("auto_apply_weak_feedback"));
    assert!(json.contains("s3_trend_window"));
}

#[test]
fn style_config_defaults() {
    let cfg = RamariaConfig::default();
    // 默认开启全链路（自动为主可配置）；关闭时回退 v1.6 prompt
    assert!(cfg.style.enabled);
    assert!(cfg.style.auto_translate);
    // 显著性判定阈值（D-V17-003 / v3.1 §7.2）
    assert_eq!(cfg.style.min_sample_count, 200);
    assert_eq!(cfg.style.top_n, 10);
    assert!((cfg.style.relative_boost_ratio - 2.0).abs() < f64::EPSILON);
    assert_eq!(cfg.style.min_frequency, 5);
    assert!((cfg.style.z_critical - 2.0).abs() < f64::EPSILON);
}

#[test]
fn feedback_config_defaults() {
    let cfg = RamariaConfig::default();
    // 默认开启采集，auto_apply 默认 false（回归红线 5：关闭时零自动修改）
    assert!(cfg.feedback.enabled);
    assert!(
        !cfg.feedback.auto_apply_weak_feedback,
        "auto_apply 默认关闭"
    );
    // 检测窗口 60s / 去重窗口 30s
    assert_eq!(cfg.feedback.correction_window_ms, 60_000);
    assert_eq!(cfg.feedback.continue_window_ms, 60_000);
    assert_eq!(cfg.feedback.dedup_window_ms, 30_000);
    // S3 趋势窗口 20，连续 ≥5 继续后 4 次不继续
    assert_eq!(cfg.feedback.s3_trend_window, 20);
    assert_eq!(cfg.feedback.s3_continue_trigger, 5);
    assert_eq!(cfg.feedback.s3_stop_trigger, 4);
}

#[test]
fn feedback_config_disabled_zero_auto_modify() {
    // 关闭 auto_apply：弱信号不自动修改规则/画像
    let mut cfg = RamariaConfig::default();
    cfg.feedback.auto_apply_weak_feedback = false;
    assert!(!cfg.feedback.auto_apply_weak_feedback);
    // 其余参数保持默认可独立配置
    assert_eq!(cfg.feedback.correction_window_ms, 60_000);
}

#[test]
fn feedback_config_toml_partial_override() {
    // 只配置部分键 → 缺失字段回退默认值
    let toml_text = r#"
[feedback]
enabled = false
"#;
    let cfg: RamariaConfig = toml::from_str(toml_text).expect("部分配置应可解析");
    assert!(!cfg.feedback.enabled);
    // 未配置字段使用默认值
    assert!(!cfg.feedback.auto_apply_weak_feedback);
    assert_eq!(cfg.feedback.continue_window_ms, 60_000);
}

// =========================================================
// 注入层运行时间门（InjectionGate，探针消融专用）
// =========================================================

/// 默认全开：无覆盖时对话管线注入行为与既有版本一致（回归红线）。
#[test]
fn injection_gate_defaults_all_on() {
    let g = InjectionGate::default();
    assert!(g.behavior);
    assert!(g.knowledge);
    assert!(g.speaking_style);
    assert!(g.examples);
    assert!(g.utt);
    assert!(g.narrative);
    assert!(g.bridge);
    assert!(g.memory_rag);
    let cfg = RamariaConfig::default();
    assert!(
        cfg.injection.behavior && cfg.injection.memory_rag,
        "默认配置闸门全开"
    );
}

/// 全关（B0 基座）与全开互为补集。
#[test]
fn injection_gate_off_is_complement_of_on() {
    let on = InjectionGate::all_on();
    let off = InjectionGate::all_off();
    assert!(!off.behavior && !off.memory_rag && !off.narrative);
    assert!(on.behavior && on.memory_rag && on.narrative);
}

/// 社交对话基调是全局体裁约束、非记忆层闸门：全关档位（B0 等消融）
/// 仍保持注入，全开与默认同样开启（保证既有消融语义与历史可比）。
#[test]
fn injection_gate_social_tone_survives_all_off() {
    assert!(
        InjectionGate::all_off().social_tone,
        "全关档位不得关闭全局社交对话基调"
    );
    assert!(
        InjectionGate::all_on().social_tone,
        "全开档位应注入全局社交对话基调"
    );
    assert!(
        InjectionGate::default().social_tone,
        "默认闸门应注入全局社交对话基调"
    );
}

/// 闸门不写入持久化：JSON/TOML 序列化不含 injection 键，
/// 反序列化回退默认全开（保持配置文件与 DB 键集稳定）。
#[test]
fn injection_gate_is_memory_only_not_persisted() {
    let mut cfg = RamariaConfig::default();
    cfg.injection.memory_rag = false;
    cfg.injection.behavior = false;

    // JSON 通道（backend_config / 信封等）：顶层无 `injection` 键。
    // 注意不能用裸 "injection" 断言（`online_memory_injection` 亦含该子串）。
    let json = serde_json::to_string(&cfg).unwrap();
    let parsed_json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        parsed_json.get("injection").is_none(),
        "injection 闸门不得序列化到 JSON"
    );
    let back_json: RamariaConfig = serde_json::from_str(&json).unwrap();
    assert!(back_json.injection.memory_rag, "反序列化后闸门回退全开");

    // TOML 通道（config.toml / ConfigSyncService）：无 `[injection]` 表。
    let toml_text = toml::to_string(&cfg).unwrap();
    assert!(
        !toml_text.contains("[injection]"),
        "injection 闸门不得序列化到 TOML"
    );
    let back_toml: RamariaConfig = toml::from_str(&toml_text).unwrap();
    assert!(back_toml.injection.behavior, "TOML 反序列化后闸门回退全开");
}

/// 层间去重默认关闭：序列化/反序列化后仍关闭（默认关闭 = 回退既有路径）。
#[test]
fn layer_dedup_default_off_and_survives_roundtrip() {
    let cfg = RamariaConfig::default();
    assert!(
        !cfg.layer_dedup.enabled,
        "层间去重默认关闭（回退既有知识层引用级去重）"
    );

    let json = serde_json::to_string(&cfg).unwrap();
    let back_json: RamariaConfig = serde_json::from_str(&json).unwrap();
    assert!(!back_json.layer_dedup.enabled, "JSON 往返后保持关闭");

    let toml_text = toml::to_string(&cfg).unwrap();
    let back_toml: RamariaConfig = toml::from_str(&toml_text).unwrap();
    assert!(!back_toml.layer_dedup.enabled, "TOML 往返后保持关闭");

    // 显式开启后 JSON 往返保持开启（配置可被 CLI/评估覆盖）
    let mut on = RamariaConfig::default();
    on.layer_dedup.enabled = true;
    let json_on = serde_json::to_string(&on).unwrap();
    let back_on: RamariaConfig = serde_json::from_str(&json_on).unwrap();
    assert!(back_on.layer_dedup.enabled, "显式开启应持久化");
}

#[test]
fn style_config_disabled_falls_back_to_v16() {
    // 关闭风格统计：整链路回退 v1.6（prompt 不含自动风格规则）
    let mut cfg = RamariaConfig::default();
    cfg.style.enabled = false;
    assert!(!cfg.style.enabled);
    // 其他阈值保持默认可独立配置
    assert_eq!(cfg.style.min_sample_count, 200);
}

// =========================================================
// v1.4 新增配置组测试（[utt] / [examples] / [bridge]）
// =========================================================

#[test]
fn utt_config_defaults() {
    let cfg = RamariaConfig::default();

    // 开关默认开启
    assert!(cfg.utt.enabled);
    // 切分参数（10/80）
    assert_eq!(cfg.utt.theta_gap_minutes, 10);
    assert_eq!(cfg.utt.max_msgs_per_block, 80);
    // 检索与预算
    assert_eq!(cfg.utt.retrieve_top_k, 3);
    assert_eq!(cfg.utt.max_block_chars, 1500);
    // 默认白名单 = 角色类 persona（char/anim/oc/hist）
    let expected = [
        PersonaKind::Char,
        PersonaKind::Anim,
        PersonaKind::Oc,
        PersonaKind::Hist,
    ];
    assert_eq!(cfg.utt.persona_kind_whitelist, expected);
    // 助手/系统类不在默认白名单中（助手类不注入原文）
    assert!(!cfg.utt.persona_kind_whitelist.contains(&PersonaKind::Rama));
    assert!(!cfg.utt.persona_kind_whitelist.contains(&PersonaKind::User));
}

#[test]
fn examples_config_defaults() {
    let cfg = RamariaConfig::default();
    assert!(cfg.examples.enabled);
    assert_eq!(cfg.examples.max_examples, 5);
}

#[test]
fn bridge_config_defaults() {
    let cfg = RamariaConfig::default();
    assert!(cfg.bridge.enabled);
    assert_eq!(cfg.bridge.max_chars, 800);
}

#[test]
fn v14_config_groups_serde_roundtrip() {
    // JSON 往返：新配置组序列化/反序列化保持一致
    let cfg = RamariaConfig::default();
    let json = serde_json::to_string(&cfg).unwrap();
    let back: RamariaConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(back.utt.theta_gap_minutes, cfg.utt.theta_gap_minutes);
    assert_eq!(
        back.utt.persona_kind_whitelist,
        cfg.utt.persona_kind_whitelist
    );
    assert_eq!(back.examples.max_examples, cfg.examples.max_examples);
    assert_eq!(back.bridge.max_chars, cfg.bridge.max_chars);
    assert_eq!(back.bridge.enabled, cfg.bridge.enabled);
}

#[test]
fn v14_config_groups_toml_roundtrip() {
    // TOML 往返（config.toml 通道）：新配置组可经 toml 文本无损恢复
    let cfg = RamariaConfig::default();
    let toml_text = toml::to_string(&cfg).expect("默认配置应可序列化为 TOML");
    let back: RamariaConfig = toml::from_str(&toml_text).expect("默认 TOML 应可反序列化");

    assert_eq!(back.utt.enabled, cfg.utt.enabled);
    assert_eq!(back.utt.theta_gap_minutes, cfg.utt.theta_gap_minutes);
    assert_eq!(back.utt.max_msgs_per_block, cfg.utt.max_msgs_per_block);
    assert_eq!(back.utt.retrieve_top_k, cfg.utt.retrieve_top_k);
    assert_eq!(back.utt.max_block_chars, cfg.utt.max_block_chars);
    assert_eq!(
        back.utt.persona_kind_whitelist,
        cfg.utt.persona_kind_whitelist
    );
    assert_eq!(back.examples.enabled, cfg.examples.enabled);
    assert_eq!(back.examples.max_examples, cfg.examples.max_examples);
    assert_eq!(back.bridge.enabled, cfg.bridge.enabled);
    assert_eq!(back.bridge.max_chars, cfg.bridge.max_chars);
}

#[test]
fn v14_config_groups_missing_fields_fallback_to_defaults() {
    // 兼容性：旧配置文件（无 [utt]/[examples]/[bridge]）解析后回退默认值
    let legacy_toml = r#"
version = "1.4.0"
schema_version = 1
[backend]
provider = "lm-studio"
"#;
    let cfg: RamariaConfig = toml::from_str(legacy_toml).expect("旧配置应可解析");
    assert!(cfg.utt.enabled, "缺失 [utt] 组应回退默认值");
    assert_eq!(cfg.utt.theta_gap_minutes, 10);
    assert_eq!(cfg.examples.max_examples, 5);
    assert!(cfg.bridge.enabled);
}

/// 六组基础配置（paths/decay/session/thresholds/index/logging）只写部分键时，
/// 缺失字段回退 `Default`，而不是让 `toml::from_str` 报 missing field
/// （后者会导致 config_sync 整体回退默认，用户显式键全部失效）。
#[test]
fn partial_group_keys_fall_back_to_defaults() {
    let toml_text = r#"
[paths]
data_dir = "D:/ramaria/data"

[decay]
s_l0 = 5

[session]
l1_idle_minutes = 30

[thresholds]
l2_trigger_count = 9

[index]
bm25_incremental_threshold = 20

[logging]
log_full_prompt = true
"#;
    let cfg: RamariaConfig = toml::from_str(toml_text).expect("部分键配置应可解析");

    // 路径组：显式键生效，其余键回退默认
    assert_eq!(cfg.paths.data_dir, "D:/ramaria/data");
    assert_eq!(cfg.paths.config_dir, "");

    // 衰减组
    assert_eq!(cfg.decay.s_l0, 5);
    assert_eq!(cfg.decay.s_l1, 30);
    assert!(cfg.decay.enable_access_boost);

    // Session 组
    assert_eq!(cfg.session.l1_idle_minutes, 30);
    assert_eq!(cfg.session.max_history_messages, 200);
    assert_eq!(cfg.session.max_history_chars, 6000);

    // 阈值组：缺 cluster_delay_ms 必须回退默认 800，而不是 serde 裸 default 的 0
    assert_eq!(cfg.thresholds.l2_trigger_count, 9);
    assert_eq!(cfg.thresholds.l3_trigger_count, 10);
    assert_eq!(
        cfg.thresholds.cluster_delay_ms, 800,
        "缺 cluster_delay_ms 应回退 800，而非 0"
    );

    // 索引组
    assert_eq!(cfg.index.bm25_incremental_threshold, 20);
    assert_eq!(cfg.index.bm25_rebuild_interval, 300);

    // 日志组
    assert!(cfg.logging.log_full_prompt);
}

/// 显式配置 `cluster_delay_ms = 0` 仍被尊重（0 = 不等待，是合法用户值）。
#[test]
fn explicit_zero_cluster_delay_is_respected() {
    let cfg: RamariaConfig =
        toml::from_str("[thresholds]\ncluster_delay_ms = 0\n").expect("显式 0 应可解析");
    assert_eq!(cfg.thresholds.cluster_delay_ms, 0);
}

#[test]
fn v14_config_groups_partial_override() {
    // 部分覆盖：只配置 [utt] 的 enabled=false，其余字段回退默认
    let partial_toml = r#"
[utt]
enabled = false
"#;
    let cfg: RamariaConfig = toml::from_str(partial_toml).expect("部分配置应可解析");
    assert!(!cfg.utt.enabled);
    // 未配置字段使用默认值
    assert_eq!(cfg.utt.theta_gap_minutes, 10);
    assert_eq!(cfg.utt.persona_kind_whitelist.len(), 4);
    assert_eq!(cfg.examples.max_examples, 5);
}

#[test]
fn v14_whitelist_serde_string_form() {
    // config.toml 中以字符串数组书写白名单（PersonaKind lowercase 序列化）
    let toml_text = r#"
[utt]
persona_kind_whitelist = ["char", "anim", "oc", "hist"]
"#;
    let cfg: RamariaConfig = toml::from_str(toml_text).expect("白名单应可解析");
    assert_eq!(
        cfg.utt.persona_kind_whitelist,
        vec![
            PersonaKind::Char,
            PersonaKind::Anim,
            PersonaKind::Oc,
            PersonaKind::Hist
        ]
    );
}

// =========================================================
// 注入协调预算（[injection_budget]）配置测试
// =========================================================

#[test]
fn injection_budget_defaults_disabled() {
    let cfg = RamariaConfig::default();
    assert!(
        !cfg.injection_budget.enabled,
        "协调预算默认关闭（v1.7 等价）"
    );
    assert_eq!(cfg.injection_budget.max_injection_tokens, 1000);
    assert_eq!(
        cfg.injection_budget.max_rag_tokens, 0,
        "0 = 无独立 RAG 上限"
    );
    assert_eq!(
        cfg.injection_budget.order,
        vec![
            InjectionSlot::Rag,
            InjectionSlot::Behavior,
            InjectionSlot::Knowledge,
            InjectionSlot::Style,
            InjectionSlot::Memory,
        ],
        "默认保留顺序：RAG 基座 > 行为 > 知识 > 表达 > 脉络"
    );
}

#[test]
fn injection_budget_toml_roundtrip_and_partial() {
    // 旧/缺省配置文件（无 [injection_budget]）解析后回退默认（机制关闭）
    let legacy = r#"
version = "1.7.0"
schema_version = 1
"#;
    let cfg: RamariaConfig = toml::from_str(legacy).expect("旧配置应可解析");
    assert!(!cfg.injection_budget.enabled);
    assert_eq!(cfg.injection_budget.max_injection_tokens, 1000);

    // 显式开启（自定义上限与顺序）可经 TOML 无损恢复
    let toml_text = r#"
[injection_budget]
enabled = true
max_injection_tokens = 800
max_rag_tokens = 300
order = ["behavior", "knowledge", "rag", "style", "memory"]
"#;
    let cfg2: RamariaConfig = toml::from_str(toml_text).expect("协调预算 TOML 应可解析");
    assert!(cfg2.injection_budget.enabled);
    assert_eq!(cfg2.injection_budget.max_injection_tokens, 800);
    assert_eq!(cfg2.injection_budget.max_rag_tokens, 300);
    assert_eq!(
        cfg2.injection_budget.order,
        vec![
            InjectionSlot::Behavior,
            InjectionSlot::Knowledge,
            InjectionSlot::Rag,
            InjectionSlot::Style,
            InjectionSlot::Memory,
        ]
    );

    // serde JSON 往返保持类型
    let json = serde_json::to_string(&cfg2).unwrap();
    let back: RamariaConfig = serde_json::from_str(&json).unwrap();
    assert!(back.injection_budget.enabled);
    assert_eq!(back.injection_budget.order, cfg2.injection_budget.order);
}

#[test]
fn injection_budget_flat_map_includes_group() {
    // config_sync 扁平化应自动覆盖本组（与 knowledge/style 同机制）
    let cfg = RamariaConfig::default();
    let flat = config_sync_flatten(&cfg);
    assert_eq!(
        flat.get("injection_budget.enabled"),
        Some(&serde_json::json!(false)),
        "协调预算组应参与 DB settings 扁平同步"
    );
    assert_eq!(
        flat.get("injection_budget.max_injection_tokens"),
        Some(&serde_json::json!(1000))
    );
}

/// 提取 config_sync 使用的扁平化逻辑（避免跨 crate 依赖，保持本文件自洽）。
fn config_sync_flatten(
    cfg: &RamariaConfig,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    use std::collections::BTreeMap;
    let mut out = BTreeMap::new();
    let Ok(root) = serde_json::to_value(cfg) else {
        return out;
    };
    let skip = ["version", "schema_version", "paths", "backend", "injection"];
    let Some(obj) = root.as_object() else {
        return out;
    };
    fn flatten(
        prefix: &str,
        value: &serde_json::Value,
        out: &mut BTreeMap<String, serde_json::Value>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    let key = format!("{prefix}.{k}");
                    flatten(&key, v, out);
                }
            }
            _ => {
                out.insert(prefix.to_string(), value.clone());
            }
        }
    }
    for (group, value) in obj {
        if skip.contains(&group.as_str()) {
            continue;
        }
        flatten(group, value, &mut out);
    }
    out
}

// =========================================================
// MCP 接入（[mcp]）配置测试
// =========================================================

#[test]
fn mcp_config_defaults_follow_decisions() {
    let cfg = RamariaConfig::default();
    // 总开关默认关闭：未显式开启时 MCP 能力不可用，对既有功能零影响
    assert!(!cfg.mcp.enabled, "MCP 接入默认关闭");
    // 写侧默认放开（回流闭环开箱可用），封存为独立开关
    assert!(cfg.mcp.allow_ingest, "回流写入默认开启");
    assert!(cfg.mcp.allow_seal, "MCP 侧封存默认允许");
    // 读侧默认保守：全部人格可见 + 原文块关闭
    assert_eq!(cfg.mcp.allowed_personas, vec!["*".to_string()]);
    assert!(!cfg.mcp.allow_raw_text, "原文块默认不出端");
    // 召回默认预算与服务层默认常量同口径（5 / 1200）
    assert_eq!(cfg.mcp.max_items, 5);
    assert_eq!(cfg.mcp.max_chars, 1200);
}

#[test]
fn mcp_config_toml_roundtrip_and_partial() {
    // 旧配置文件（无 [mcp]）解析后回退默认（未开启）
    let legacy = r#"
version = "2.0.0"
schema_version = 1
"#;
    let cfg: RamariaConfig = toml::from_str(legacy).expect("旧配置应可解析");
    assert!(!cfg.mcp.enabled);

    // 显式配置可无损恢复；只写部分键时其余键回退默认值
    let toml_text = r#"
[mcp]
enabled = true
allowed_personas = ["rama-0001"]
allow_raw_text = true
"#;
    let cfg2: RamariaConfig = toml::from_str(toml_text).expect("MCP 配置应可解析");
    assert!(cfg2.mcp.enabled);
    assert_eq!(cfg2.mcp.allowed_personas, vec!["rama-0001".to_string()]);
    assert!(cfg2.mcp.allow_raw_text);
    assert!(cfg2.mcp.allow_ingest, "未写的键回退默认值");
    assert_eq!(cfg2.mcp.max_items, 5);

    // 扁平化同步覆盖本组（settings 表 config.* 键）
    let flat = config_sync_flatten(&cfg2);
    assert_eq!(flat.get("mcp.enabled"), Some(&serde_json::json!(true)));
    assert_eq!(
        flat.get("mcp.allow_seal"),
        Some(&serde_json::json!(true)),
        "MCP 组应参与 DB settings 扁平同步"
    );
}

// =========================================================
// 主动对话（[proactive]）配置测试
// =========================================================

#[test]
fn proactive_config_defaults_follow_decisions() {
    let cfg = RamariaConfig::default();
    // 总开关默认开启（打扰控制由各键约束）
    assert!(cfg.proactive.enabled, "主动对话默认开启");
    // 调度节拍与打扰控制默认值逐键锁定
    assert_eq!(cfg.proactive.check_interval_seconds, 300);
    assert_eq!(cfg.proactive.min_idle_hours, 4);
    assert_eq!(cfg.proactive.daily_limit, 3);
    assert_eq!(cfg.proactive.quiet_hours, "22:00-08:00");
    assert_eq!(cfg.proactive.cooldown_hours, 8);
    // 判据与软加权算法参数默认值逐键锁定
    assert!(cfg.proactive.judge_enabled, "判据默认开启");
    assert_eq!(cfg.proactive.judge_interval_hours, 3);
    assert!((cfg.proactive.active_hours_weight - 0.8).abs() < f64::EPSILON);
    assert_eq!(cfg.proactive.active_hours_window_days, 30);
    assert_eq!(cfg.proactive.active_hours_min_samples, 50);
    assert!((cfg.proactive.valence_weight - 0.5).abs() < f64::EPSILON);
    assert!((cfg.proactive.confidence_floor - 0.6).abs() < f64::EPSILON);
    assert!((cfg.proactive.light_touch_weight - 0.3).abs() < f64::EPSILON);
    // 事件源阈值与选题去重冷却默认值逐键锁定
    assert!((cfg.proactive.event_salience_threshold - 0.6).abs() < f64::EPSILON);
    assert_eq!(cfg.proactive.event_window_days, 14);
    assert!((cfg.proactive.unresolved_valence_threshold - (-0.3)).abs() < f64::EPSILON);
    assert_eq!(cfg.proactive.follow_up_days, 3);
    assert_eq!(cfg.proactive.topic_cooldown_hours, 24);
    assert_eq!(cfg.proactive.silence_backoff_days, 3);
    assert_eq!(cfg.proactive.startup_grace_days, 3);
}

#[test]
fn proactive_config_toml_roundtrip_and_partial() {
    // 旧配置文件（无 [proactive]）解析后回退默认（开启、默认值）
    let legacy = r#"
version = "2.0.0"
schema_version = 1
"#;
    let cfg: RamariaConfig = toml::from_str(legacy).expect("旧配置应可解析");
    assert!(cfg.proactive.enabled, "缺 [proactive] 时回退默认开启");
    assert_eq!(cfg.proactive.check_interval_seconds, 300);

    // 显式配置可无损恢复；只写部分键时其余键回退默认值
    let toml_text = r#"
[proactive]
enabled = false
daily_limit = 2
quiet_hours = "23:00-07:30"
"#;
    let cfg2: RamariaConfig = toml::from_str(toml_text).expect("主动对话配置应可解析");
    assert!(!cfg2.proactive.enabled);
    assert_eq!(cfg2.proactive.daily_limit, 2);
    assert_eq!(cfg2.proactive.quiet_hours, "23:00-07:30");
    assert_eq!(
        cfg2.proactive.cooldown_hours, 8,
        "未写的键回退默认值（冷却 8 小时）"
    );
    // 未写的判据与软加权键同样回退默认值
    assert!(cfg2.proactive.judge_enabled);
    assert_eq!(cfg2.proactive.judge_interval_hours, 3);
    assert_eq!(cfg2.proactive.active_hours_window_days, 30);
    assert_eq!(cfg2.proactive.active_hours_min_samples, 50);
    assert!((cfg2.proactive.confidence_floor - 0.6).abs() < f64::EPSILON);

    // 扁平化同步覆盖本组（settings 表 config.* 键）
    let flat = config_sync_flatten(&cfg2);
    assert_eq!(
        flat.get("proactive.enabled"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        flat.get("proactive.daily_limit"),
        Some(&serde_json::json!(2)),
        "主动对话组应参与 DB settings 扁平同步"
    );
}

// =========================================================
// 图片理解（[vision]）配置测试
// =========================================================

#[test]
fn vision_config_defaults_follow_decisions() {
    let cfg = RamariaConfig::default();
    // 能力声明默认关闭：未显式声明可识图时图片理解整体跳过
    assert!(
        !cfg.vision.model_supports_vision,
        "图片识别能力声明默认关闭"
    );
    // 单批上限默认 0 = 不限
    assert_eq!(cfg.vision.batch_limit, 0, "单批理解上限默认不限");
}

#[test]
fn vision_config_toml_roundtrip_and_partial() {
    // 旧配置文件（无 [vision]）解析后回退默认（声明关闭、上限不限）
    let legacy = r#"
version = "2.0.0"
schema_version = 1
"#;
    let cfg: RamariaConfig = toml::from_str(legacy).expect("旧配置应可解析");
    assert!(
        !cfg.vision.model_supports_vision,
        "缺 [vision] 时回退默认关闭"
    );
    assert_eq!(cfg.vision.batch_limit, 0);

    // 显式配置可无损恢复；只写部分键时其余键回退默认值
    let toml_text = r#"
[vision]
model_supports_vision = true
"#;
    let cfg2: RamariaConfig = toml::from_str(toml_text).expect("图片理解配置应可解析");
    assert!(cfg2.vision.model_supports_vision);
    assert_eq!(cfg2.vision.batch_limit, 0, "未写的键回退默认值");

    // 扁平化同步覆盖本组（settings 表 config.* 键）
    let flat = config_sync_flatten(&cfg2);
    assert_eq!(
        flat.get("vision.model_supports_vision"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        flat.get("vision.batch_limit"),
        Some(&serde_json::json!(0)),
        "图片理解组应参与 DB settings 扁平同步"
    );
}
