//! crates/ramaria-memory/src/fact/extractor/tests.rs - //! crates/ramaria-memory/src/fact/extractor.rs - 知识层事实抽取单元测试
//!
//! 设计特点:
//! - 位于 fact::extractor 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 extractor.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::types::MemoryEvent;

fn event(id: i64, title: &str, summary: &str, kw: &str, conf: f64) -> MemoryEvent {
    let mut e = MemoryEvent::new("char-0001".into(), title.into(), summary.into(), 0, 1000);
    e.id = id;
    e.keywords = Some(kw.to_string());
    e.confidence = conf;
    e.presentation = Presentation::Mixed;
    e
}

#[test]
fn objective_fact_extracts_regular_candidate() {
    let ev = event(1, "加入开源项目", "正在参与项目开发", "工作,项目,开源", 0.9);
    let candidates = RuleExtractor::extract_from_event(&ev);
    assert!(!candidates.is_empty());
    let regular = candidates.iter().find(|c| !c.subjective_implied).unwrap();
    assert_eq!(regular.field, ProfileField::PersonalStatus);
    assert!(regular.confidence >= EXTRACT_CONFIDENCE_THRESHOLD);
    assert_eq!(regular.source, FactSource::Event);
}

#[test]
fn subjective_event_yields_implied_fact() {
    let mut ev = event(2, "情绪低落", "最近压力大", "压力,情绪", 0.7);
    ev.presentation = Presentation::Subjective;
    let candidates = RuleExtractor::extract_from_event(&ev);
    let implied = candidates.iter().find(|c| c.subjective_implied);
    assert!(implied.is_some(), "主观事件应产出隐含偏好事实");
    let imp = implied.unwrap();
    assert_eq!(imp.confidence, SUBJECTIVE_IMPLIED_CONFIDENCE);
    assert!(imp.content.contains("偏好"));
}

#[test]
fn below_threshold_event_only_implied() {
    // conf < 0.6 → 不触发常规事实，但可进入隐含偏好轨道
    let ev = event(3, "小事", "一般", "日常", 0.4);
    let candidates = RuleExtractor::extract_from_event(&ev);
    assert!(!candidates.is_empty());
    assert!(candidates.iter().all(|c| c.subjective_implied));
}

#[test]
fn would_outline_classify_interests() {
    let ev = event(4, "喜欢看科幻电影", "经常看", "喜欢,科幻", 0.9);
    let candidates = RuleExtractor::extract_from_event(&ev);
    let regular = candidates.iter().find(|c| !c.subjective_implied).unwrap();
    assert_eq!(regular.field, ProfileField::Interests);
    assert!(!regular.keywords.is_empty());
}

#[test]
fn empty_content_skips() {
    let ev = event(5, "", "", "无", 0.9);
    let candidates = RuleExtractor::extract_from_event(&ev);
    // 标题为空且无 paraphrase → 无候选
    assert!(candidates.is_empty() || candidates.iter().all(|c| c.confidence < 0.6));
}

#[test]
fn prompt_builds_structure() {
    let prompt = build_extract_prompt("事件描述", "小明");
    assert!(prompt.contains("小明"));
    assert!(prompt.contains("content"));
    assert!(prompt.contains("JSON"));
}

// =========================================================
// 策略① 主观隐含事实补全（字段感知增强层）
// =========================================================

/// 主观 Interests 事件 → "偏好：" 前缀 + Interests/Stable/conf=0.5。
#[test]
fn implied_fact_maps_interests_prefix() {
    let mut ev = event(
        10,
        "好喜欢看科幻电影",
        "提到喜欢科幻和悬疑",
        "喜欢,科幻",
        0.7,
    );
    ev.presentation = Presentation::Subjective;
    let cand = extract_implied_fact_from_event(&ev).expect("主观兴趣事件应产出隐含事实");
    assert_eq!(cand.field, ProfileField::Interests);
    assert_eq!(cand.tier, FactTier::Stable);
    assert!(
        cand.content.starts_with("偏好："),
        "content={}",
        cand.content
    );
    assert_eq!(cand.confidence, SUBJECTIVE_IMPLIED_CONFIDENCE);
    assert!(cand.subjective_implied);
    assert_eq!(cand.ref_event_id, Some(10));
    assert!(cand.ref_l1_id.is_none());
}

/// 主观 Social 事件 → "关系：" 前缀 + Social 字段（不只 Interests）。
#[test]
fn implied_fact_maps_social_prefix() {
    let mut ev = event(
        11,
        "说很看重和朋友的关系",
        "觉得朋友很重要",
        "朋友,关系",
        0.7,
    );
    ev.presentation = Presentation::Subjective;
    let cand = extract_implied_fact_from_event(&ev).expect("主观社交事件应产出隐含事实");
    assert_eq!(cand.field, ProfileField::Social);
    assert!(
        cand.content.starts_with("关系："),
        "content={}",
        cand.content
    );
    assert_eq!(cand.tier, FactTier::Stable);
}

/// 主观 History 事件 → "经历：" 前缀 + History 字段。
#[test]
fn implied_fact_maps_history_prefix() {
    let mut ev = event(12, "回想曾经养过猫的日子", "以前养过一只猫", "猫,曾经", 0.7);
    ev.presentation = Presentation::Subjective;
    let cand = extract_implied_fact_from_event(&ev).expect("主观历史事件应产出隐含事实");
    assert_eq!(cand.field, ProfileField::History);
    assert!(
        cand.content.starts_with("经历："),
        "content={}",
        cand.content
    );
    assert_eq!(cand.tier, FactTier::Historical);
}

/// 低置信（conf<0.6）客观事件 → "近况：" 前缀，field 按 PersonalStatus 关键词命中。
#[test]
fn implied_fact_maps_personal_status_for_low_confidence() {
    let ev = event(13, "最近加班多", "最近工作压力大", "工作,加班", 0.4);
    let cand = extract_implied_fact_from_event(&ev).expect("低置信事件应产出隐含事实");
    assert_eq!(cand.field, ProfileField::PersonalStatus);
    assert_eq!(cand.tier, FactTier::Volatile);
    assert!(
        cand.content.starts_with("近况："),
        "content={}",
        cand.content
    );
}

/// 常规轨道（客观且 conf≥0.6）不产隐含事实——增强函数只服务隐含轨道。
#[test]
fn implied_fact_skips_regular_track() {
    let ev = event(
        14,
        "加入开源项目",
        "正在参与项目开发",
        "工作,项目,开源",
        0.9,
    );
    assert!(
        extract_implied_fact_from_event(&ev).is_none(),
        "常规轨道事件不应产出隐含事实"
    );
}

// =========================================================
// 策略② 线索→断言覆盖
// =========================================================

/// 构造带 id 与 persona 的 L1 夹具。
fn l1_fixture(l1_id: Uuid, persona: &str) -> MemoryL1 {
    let mut l1 = MemoryL1::new(Uuid::new_v4(), "摘要".to_string(), None);
    l1.id = l1_id;
    l1.persona_uid = Some(persona.to_string());
    l1
}

/// L1 保真线索 → 断言级候选：source=L1、ref_l1_id 溯源、conf=0.55。
#[test]
fn l1_evidence_lifts_to_assertion_candidate() {
    let l1_id = Uuid::new_v4();
    let l1 = l1_fixture(l1_id, "char-0001");
    // text 承载事实断言；time/who/cause 元数据放在槽位（验证不进 content）
    let note = EvidenceNote::with_slots(
        "每周都要加班到很晚",
        Some("最近一个月".to_string()),
        Some("用户".to_string()),
        Some("项目上线压力大".to_string()),
    );
    let cand = extract_from_l1_evidence("char-0001", &l1, &note).expect("有效线索应产出候选");
    assert_eq!(cand.source, FactSource::L1);
    assert_eq!(cand.ref_event_id, None);
    assert_eq!(cand.ref_l1_id, Some(l1_id));
    assert!(!cand.subjective_implied);
    assert_eq!(cand.confidence, L1_EVIDENCE_CONFIDENCE);
    // 时间/who/cause 仅作元数据，不进 content（避免喧宾夺主）
    assert!(
        !cand.content.contains("最近一个月"),
        "content={}",
        cand.content
    );
    assert!(!cand.content.contains("用户"), "content={}", cand.content);
    assert!(cand.content.contains("加班"), "content={}", cand.content);
    // 关键词从 text 提取（bigram 命中"加班"）
    assert!(
        cand.keywords.iter().any(|k| k == "加班"),
        "keywords={:?}",
        cand.keywords
    );
    assert!(!cand.keywords.is_empty());
}

/// 过短 text（<5 字符）→ None，对齐 L1 evidence 校验口径。
#[test]
fn l1_evidence_short_text_is_none() {
    let l1 = l1_fixture(Uuid::new_v4(), "char-0001");
    let note = EvidenceNote::new("短");
    assert!(
        extract_from_l1_evidence("char-0001", &l1, &note).is_none(),
        "过短线索不应提升为断言候选"
    );
}

/// 空白 text → None。
#[test]
fn l1_evidence_blank_text_is_none() {
    let l1 = l1_fixture(Uuid::new_v4(), "char-0001");
    let note = EvidenceNote::new("   ");
    assert!(extract_from_l1_evidence("char-0001", &l1, &note).is_none());
}

/// 超长 text 截断到字符上限且不切多字节。
#[test]
fn l1_evidence_long_text_truncated() {
    let l1 = l1_fixture(Uuid::new_v4(), "char-0001");
    let long = format!("用户喜欢{}", "长句内容".repeat(30));
    let note = EvidenceNote::new(&long);
    let cand =
        extract_from_l1_evidence("char-0001", &l1, &note).expect("超长线索仍应产出候选（截断）");
    assert!(cand.content.chars().count() <= L1_EVIDENCE_MAX_CONTENT_CHARS);
    assert!(
        cand.content.starts_with("用户喜欢"),
        "content={}",
        cand.content
    );
}

/// L1 persona 已归属他人 → 隔离拒绝（不跨 persona 泄漏）。
#[test]
fn l1_evidence_persona_mismatch_is_none() {
    let l1 = l1_fixture(Uuid::new_v4(), "char-other");
    let note = EvidenceNote::new("每周都要加班到很晚");
    assert!(
        extract_from_l1_evidence("char-0001", &l1, &note).is_none(),
        "persona 不一致应拒绝"
    );
}
