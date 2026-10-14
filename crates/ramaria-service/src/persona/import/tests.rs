//! crates/ramaria-service/src/persona/import/tests.rs - Ramaria 预设导入用例单元测试
//!
//! 设计特点:
//! - 由 `persona::import` 以 `#[cfg(test)] mod tests;` 收纳：覆盖四类写入往返 / 合并语义 /
//!   dry-run 一致性 / 拆解成功与失败降级 / 隐私未确认 / uid 自动生成 / --into 合并 /
//!   导入后端到端（提示词）/ 文件级失败
//! - 使用真实 SQLite 临时库与 mock LLM：断言以落库值与导入结果统计为准
//! - 预设文件写入系统临时目录，用例结束清理（含结果不一致等失败路径）
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问网络、OS keychain 与真实用户数据。

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_core::behavior::BehaviorParams;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, FactTier, PersonaKind, ProfileField};

use ramaria_memory::preset::{KindSource, PreviewMode, SkipReason};

use super::*;
use crate::test_support::{
    MockLlm, engine_with_db, engine_with_failing_llm, engine_with_shared_llm, seed_persona,
    temp_dir,
};
use crate::types::{CHANNEL_MCP, ChatSendRequest};

// =========================================================
// 共享夹具
// =========================================================

/// 写预设文件到独立临时目录（返回文件路径与目录句柄，调用方负责清理）。
fn write_preset(tag: &str, file_name: &str, content: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(tag);
    let path = dir.join(file_name);
    std::fs::write(&path, content).expect("写入预设文件应成功");
    (path, dir)
}

/// 完整预设：注册列 + 三种 trait 层 + 四类结构化段 + 自由文本块。
const FULL_PRESET: &str = r#"[persona]
uid = "char-0007"
kind = "char"
name = "黎杋枫"
avatar = "https://example.com/avatar.png"
description = "温和的医学生"

[identity]
assistant_name = "黎杋枫"
user_name = "用户"

[blocks]
A_persona = "角色设定文本。"

[traits]
base = [{ label = "温柔", meaning = "对他人保持温和" }]
primary = [{ label = "慢热", meaning = "需要时间放松" }]
accent = [{ label = "对昆虫着迷", meaning = "谈及昆虫兴致高昂", not_meaning = "并非对所有生物都着迷", trigger = "提及昆虫", suppress = "正式场合", related = "温柔" }]

[[facts]]
field = "BasicInfo"
content = "医学院学生"

[[examples]]
user = "今天好累啊"
assistant = "先去休息一下吧"

[[behavior_rules]]
keywords = ["加班", "工作"]
reaction = "表达关心并建议早点休息"
avoid = ["说教"]
params = { emotion_intensity = -0.3 }
"#;

/// 简单预设（dry-run 对照与端到端使用；含描述与 traits）。
const SIMPLE_PRESET: &str = r#"[persona]
uid = "char-0011"
kind = "char"
name = "预览人格"
description = "用于 dry-run 对照"

[identity]
assistant_name = "预览人格"

[blocks]
A_persona = "自由文本块。"

[traits]
base = [{ label = "沉稳", meaning = "遇事不慌" }, { label = "细心", meaning = "关注细节" }]

[[facts]]
field = "Interests"
content = "喜欢整理"

[[examples]]
user = "明天要面试了"
assistant = "别紧张，你准备得很充分"

[[behavior_rules]]
keywords = ["面试"]
reaction = "给予鼓励与具体建议"
"#;

// =========================================================
// 四类写入往返
// =========================================================

/// 完整预设（--no-ai）：四类条目按落库默认值写入，注册列逐列对齐。
#[tokio::test]
async fn import_writes_four_entry_categories() {
    let (engine, storage, dir) = engine_with_db("preset-import-roundtrip").await;
    let (path, preset_dir) = write_preset("preset-files-roundtrip", "lifanfeng.toml", FULL_PRESET);

    let outcome = engine
        .persona_import(&path, None, true)
        .await
        .expect("导入应成功");

    // 拆解：--no-ai 未请求、未执行、无跳过原因
    assert!(!outcome.decompose.requested);
    assert!(!outcome.decompose.executed);
    assert_eq!(outcome.decompose.skipped_reason, None);

    // 写入统计：四类各按其数、零失败；预览计数与实写一致
    assert_eq!(
        outcome.written,
        ImportWriteStats {
            traits: 3,
            facts: 1,
            examples: 1,
            behavior_rules: 1,
            failed: 0,
        }
    );
    assert_eq!(outcome.preview.will_write.traits.base, 1);
    assert_eq!(outcome.preview.will_write.traits.primary, 1);
    assert_eq!(outcome.preview.will_write.traits.accent, 1);
    assert_eq!(outcome.preview.will_write.facts.count, 1);
    assert_eq!(outcome.preview.will_write.examples.count, 1);
    assert_eq!(outcome.preview.will_write.behavior_rules.count, 1);
    assert_eq!(outcome.preview.will_write.blocks.segments, 1);

    // persona 注册列：uid / name / kind / source / seq / avatar / description / config 原文
    let persona = storage
        .get_persona_by_uid("char-0007")
        .await
        .expect("查询应成功")
        .expect("persona 应存在");
    assert_eq!(persona.name, "黎杋枫");
    assert_eq!(persona.kind, PersonaKind::Char);
    assert_eq!(persona.source, "manual");
    assert_eq!(persona.seq, 7, "seq 应取 uid 数字后缀");
    assert_eq!(
        persona.avatar.as_deref(),
        Some("https://example.com/avatar.png")
    );
    assert_eq!(persona.description.as_deref(), Some("温和的医学生"));
    assert_eq!(
        persona.config.as_deref(),
        Some(FULL_PRESET),
        "TOML 应存原文"
    );

    // traits：三层各一条；落库默认 source / status / confidence / evidence / consistency
    let traits = storage
        .list_traits_by_persona("char-0007")
        .await
        .expect("查询应成功");
    assert_eq!(traits.len(), 3);

    let gentle = traits
        .iter()
        .find(|t| t.trait_label == "温柔")
        .expect("应含「温柔」");
    assert_eq!(gentle.layer, TraitLayer::Base);
    assert_eq!(gentle.seq, 1, "层内声明顺序从 1 起");
    assert_eq!(gentle.source, TraitSource::Manual);
    assert_eq!(gentle.status, TraitStatus::Active);
    assert_eq!(gentle.confidence, 0.5);
    assert_eq!(gentle.evidence, 1.0);
    assert_eq!(gentle.consistency, 0.5);
    assert_eq!(gentle.not_meaning, None);

    let slow = traits
        .iter()
        .find(|t| t.trait_label == "慢热")
        .expect("应含「慢热」");
    assert_eq!(slow.layer, TraitLayer::Primary);
    assert_eq!(slow.seq, 1);

    let insect = traits
        .iter()
        .find(|t| t.trait_label == "对昆虫着迷")
        .expect("应含「对昆虫着迷」");
    assert_eq!(insect.layer, TraitLayer::Accent);
    assert_eq!(insect.seq, 1);
    assert_eq!(insect.not_meaning.as_deref(), Some("并非对所有生物都着迷"));
    assert_eq!(insect.trigger.as_deref(), Some("提及昆虫"));
    assert_eq!(insect.suppress.as_deref(), Some("正式场合"));
    assert_eq!(insect.related.as_deref(), Some("温柔"));

    // facts：source=Manual / tier 按字段分层 / confidence=1.0
    let facts = storage
        .list_all_facts_by_persona("char-0007")
        .await
        .expect("查询应成功");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].field, ProfileField::BasicInfo);
    assert_eq!(facts[0].content, "医学院学生");
    assert_eq!(facts[0].source, FactSource::Manual);
    assert_eq!(facts[0].tier, FactTier::Stable);
    assert_eq!(facts[0].confidence, 1.0);

    // examples：selected=true / tags 含关键词 / length 由构造器计算
    let examples = storage
        .list_all_examples("char-0007")
        .await
        .expect("查询应成功");
    assert_eq!(examples.len(), 1);
    assert_eq!(examples[0].partner, "今天好累啊");
    assert_eq!(examples[0].reply, "先去休息一下吧");
    assert!(examples[0].selected);
    assert_eq!(
        examples[0].length,
        "先去休息一下吧".chars().count() as i32,
        "length 应为回复字符数"
    );
    let tags = examples[0].tags.as_deref().expect("tags 应存在");
    assert!(tags.contains("今天"), "tags 应含输入关键词: {tags}");

    // rules：source=Manual / confidence=stability=1.0 / enabled / 情境与反应映射
    let rules = storage
        .list_behavior_rules_by_persona("char-0007")
        .await
        .expect("查询应成功");
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].situation.keywords,
        vec!["加班".to_string(), "工作".to_string()]
    );
    assert_eq!(rules[0].reaction.as_deref(), Some("表达关心并建议早点休息"));
    assert_eq!(rules[0].avoid, vec!["说教".to_string()]);
    assert_eq!(rules[0].source, RuleSource::Manual);
    assert_eq!(rules[0].confidence, 1.0);
    assert_eq!(rules[0].stability, 1.0);
    assert!(rules[0].enabled);
    assert!((rules[0].params.emotional_intensity + 0.3).abs() < f64::EPSILON);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// 合并语义
// =========================================================

/// 同一预设二次导入：全部跳过（四类零写入），跳过数与首次实写数逐项对应。
#[tokio::test]
async fn reimport_same_preset_skips_everything() {
    let (engine, storage, dir) = engine_with_db("preset-import-idempotent").await;
    let (path, preset_dir) = write_preset("preset-files-idempotent", "repeat.toml", FULL_PRESET);

    let first = engine
        .persona_import(&path, None, true)
        .await
        .expect("首次导入应成功");
    assert_eq!(
        first.written.traits
            + first.written.facts
            + first.written.examples
            + first.written.behavior_rules,
        6,
        "首次应写入四类共 6 条"
    );

    let second = engine
        .persona_import(&path, None, true)
        .await
        .expect("二次导入应成功");

    // 第二次全部跳过：四类零写入、零失败
    assert_eq!(second.written, ImportWriteStats::default());
    // 「将写入」与实写一致（全为 0），跳过明细逐类存在
    assert_eq!(second.preview.will_write.traits.base, 0);
    assert_eq!(second.preview.will_write.traits.primary, 0);
    assert_eq!(second.preview.will_write.traits.accent, 0);
    assert_eq!(second.preview.will_write.facts.count, 0);
    assert_eq!(second.preview.will_write.examples.count, 0);
    assert_eq!(second.preview.will_write.behavior_rules.count, 0);
    // 第二次的跳过数 = 首次实际写入数（逐项对应）
    assert_eq!(
        second.preview.will_write.traits.skipped.len(),
        first.written.traits
    );
    assert_eq!(
        second.preview.will_write.facts.skipped.len(),
        first.written.facts
    );
    assert_eq!(
        second.preview.will_write.examples.skipped.len(),
        first.written.examples
    );
    assert_eq!(
        second.preview.will_write.behavior_rules.skipped.len(),
        first.written.behavior_rules
    );
    assert!(
        second
            .preview
            .will_write
            .traits
            .skipped
            .iter()
            .all(|item| item.reason == SkipReason::LabelDuplicate)
    );
    assert!(
        second
            .preview
            .will_write
            .facts
            .skipped
            .iter()
            .all(|item| item.reason == SkipReason::Duplicate)
    );

    // 库中无重复条目
    assert_eq!(
        storage
            .list_traits_by_persona("char-0007")
            .await
            .expect("查询应成功")
            .len(),
        3
    );
    assert_eq!(
        storage
            .list_all_facts_by_persona("char-0007")
            .await
            .expect("查询应成功")
            .len(),
        1
    );
    assert_eq!(
        storage
            .list_all_examples("char-0007")
            .await
            .expect("查询应成功")
            .len(),
        1
    );
    assert_eq!(
        storage
            .list_behavior_rules_by_persona("char-0007")
            .await
            .expect("查询应成功")
            .len(),
        1
    );

    // 合并提示：uid 已存在按合并处理 + kind 不可变更
    assert!(
        second
            .preview
            .hints
            .iter()
            .any(|hint| hint.code == "merge_mode")
    );
    assert!(
        second
            .preview
            .hints
            .iter()
            .any(|hint| hint.code == "kind_immutable")
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

/// 目标已有一条各类条目：重复跳过、新条目写入、注册列只补空缺。
#[tokio::test]
async fn merge_only_fills_gaps_and_keeps_existing() {
    let (engine, storage, dir) = engine_with_db("preset-import-merge").await;

    // 预置目标人格：名称 / 配置已有；四类各有一条与预设重复的条目
    let mut persona = Persona::new(
        "char-0010".to_string(),
        "既有名字".to_string(),
        PersonaKind::Char,
        10,
        "local".to_string(),
    );
    persona.config = Some("既有配置文本".to_string());
    storage
        .create_persona(&persona)
        .await
        .expect("预置 persona 应成功");
    let existing_trait = PersonalityTrait::new(
        "char-0010".to_string(),
        TraitLayer::Base,
        "温柔".to_string(),
        "既有含义".to_string(),
        TraitSource::Manual,
        1,
    );
    storage
        .save_trait(&existing_trait)
        .await
        .expect("预置 trait 应成功");
    storage
        .save_fact(&PersonaFact::new(
            "char-0010".to_string(),
            ProfileField::BasicInfo,
            "医学院学生".to_string(),
            FactSource::Manual,
        ))
        .await
        .expect("预置 fact 应成功");
    storage
        .save_example(&PersonaExample::new(
            "char-0010".to_string(),
            "今天好累啊".to_string(),
            "先去休息一下吧".to_string(),
        ))
        .await
        .expect("预置 example 应成功");
    let existing_rule = BehaviorRule::new(
        "char-0010",
        BehaviorSituation {
            keywords: vec!["加班".to_string(), "工作".to_string()],
            ..BehaviorSituation::empty()
        },
        Some("表达关心并建议早点休息".to_string()),
        BehaviorParams::default(),
        RuleSource::Manual,
    );
    storage
        .save_behavior_rule(&existing_rule)
        .await
        .expect("预置 rule 应成功");

    let (path, preset_dir) = write_preset(
        "preset-files-merge",
        "partial.toml",
        r#"[persona]
uid = "char-0010"
kind = "char"
name = "新名字"
description = "补充描述"

[identity]
assistant_name = "新名字"

[traits]
base = [{ label = "温柔", meaning = "重复标签" }, { label = "新标签", meaning = "新增职责" }]

[[facts]]
field = "BasicInfo"
content = "医学院学生"

[[facts]]
field = "Interests"
content = "喜欢下棋"

[[examples]]
user = "今天好累啊"
assistant = "先去休息一下吧"

[[examples]]
user = "周末干什么"
assistant = "一起去看电影吧"

[[behavior_rules]]
keywords = ["工作", "加班"]
reaction = "表达关心并建议早点休息"

[[behavior_rules]]
keywords = ["考试"]
reaction = "给予鼓励"
"#,
    );

    let outcome = engine
        .persona_import(&path, None, true)
        .await
        .expect("合并导入应成功");

    // 逐类：重复跳过、新增写入；预览计数与实写一致
    assert_eq!(
        outcome.written,
        ImportWriteStats {
            traits: 1,
            facts: 1,
            examples: 1,
            behavior_rules: 1,
            failed: 0,
        }
    );
    assert_eq!(
        outcome.preview.will_write.traits.base,
        outcome.written.traits
    );
    assert_eq!(
        outcome.preview.will_write.facts.count,
        outcome.written.facts
    );
    assert_eq!(
        outcome.preview.will_write.examples.count,
        outcome.written.examples
    );
    assert_eq!(
        outcome.preview.will_write.behavior_rules.count,
        outcome.written.behavior_rules
    );
    assert_eq!(outcome.preview.will_write.traits.skipped.len(), 1);
    assert_eq!(
        outcome.preview.will_write.traits.skipped[0]
            .label
            .as_deref(),
        Some("温柔")
    );
    assert_eq!(outcome.preview.will_write.facts.skipped.len(), 1);
    assert_eq!(outcome.preview.will_write.examples.skipped.len(), 1);
    assert_eq!(outcome.preview.will_write.behavior_rules.skipped.len(), 1);

    // 注册列只补空缺：名称保持不变、描述补入、配置不覆盖
    let merged = storage
        .get_persona_by_uid("char-0010")
        .await
        .expect("查询应成功")
        .expect("persona 应存在");
    assert_eq!(merged.name, "既有名字", "名称已有值不应被覆盖");
    assert_eq!(
        merged.description.as_deref(),
        Some("补充描述"),
        "描述空缺应补入"
    );
    assert_eq!(
        merged.config.as_deref(),
        Some("既有配置文本"),
        "配置不应被覆盖"
    );
    assert!(
        outcome
            .preview
            .hints
            .iter()
            .any(|hint| hint.code == "config_skipped")
    );

    // 新增条目落库、重复条目未重复；既有「温柔」含义未被覆盖、seq 按层续号
    let traits = storage
        .list_traits_by_persona("char-0010")
        .await
        .expect("查询应成功");
    assert_eq!(traits.len(), 2);
    let added = traits
        .iter()
        .find(|t| t.trait_label == "新标签")
        .expect("应含「新标签」");
    assert_eq!(added.seq, 2, "层内 seq 应从既有最大值续号");
    let gentle = traits
        .iter()
        .find(|t| t.trait_label == "温柔")
        .expect("应含既有「温柔」");
    assert_eq!(gentle.meaning, "既有含义");
    assert_eq!(
        storage
            .list_all_facts_by_persona("char-0010")
            .await
            .expect("查询应成功")
            .len(),
        2
    );
    assert_eq!(
        storage
            .list_all_examples("char-0010")
            .await
            .expect("查询应成功")
            .len(),
        2
    );
    assert_eq!(
        storage
            .list_behavior_rules_by_persona("char-0010")
            .await
            .expect("查询应成功")
            .len(),
        2
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// dry-run 与实写一致性
// =========================================================

/// 先预览（库不变）再执行：预览结构逐项一致（含 hints），计数与实际写入相等。
#[tokio::test]
async fn dry_run_matches_execute_without_db_writes() {
    let (engine, storage, dir) = engine_with_db("preset-import-dryrun").await;
    let (path, preset_dir) = write_preset("preset-files-dryrun", "preview.toml", SIMPLE_PRESET);

    let preview = engine
        .persona_import_preview(&path, None, true)
        .await
        .expect("预览应成功");

    // 预览不写库：无 persona、无任何条目
    assert!(
        storage
            .get_persona_by_uid("char-0011")
            .await
            .expect("查询应成功")
            .is_none()
    );
    assert!(
        storage
            .list_personas()
            .await
            .expect("查询应成功")
            .is_empty()
    );
    assert_eq!(preview.registration.uid.as_deref(), Some("char-0011"));
    assert_eq!(preview.registration.mode, PreviewMode::Create);
    assert_eq!(preview.will_write.traits.base, 2);
    assert_eq!(preview.will_write.facts.count, 1);
    assert_eq!(preview.will_write.examples.count, 1);
    assert_eq!(preview.will_write.behavior_rules.count, 1);
    assert!(!preview.decompose.enabled, "--no-ai 应关闭拆解");
    assert!(!preview.decompose.privacy_confirmed);

    let outcome = engine
        .persona_import(&path, None, true)
        .await
        .expect("导入应成功");

    // 同一裁决数据源：预览结构逐项一致（含 hints）
    assert_eq!(preview, outcome.preview);
    assert_eq!(preview.hints, outcome.preview.hints);
    // 「将写入」计数 == 实际写入计数
    assert_eq!(
        outcome.preview.will_write.traits.base,
        outcome.written.traits
    );
    assert_eq!(
        outcome.preview.will_write.facts.count,
        outcome.written.facts
    );
    assert_eq!(
        outcome.preview.will_write.examples.count,
        outcome.written.examples
    );
    assert_eq!(
        outcome.preview.will_write.behavior_rules.count,
        outcome.written.behavior_rules
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// LLM 拆解（成功 / 失败降级 / 隐私未确认）
// =========================================================

/// 拆解合法 JSON（含一条与直写重复的 trait / fact，验证跨源去重）。
const DECOMPOSE_REPLY: &str = r#"{
  "facts": [
    {"field": "BasicInfo", "content": "职业是医生", "source": "Manual"},
    {"field": "Interests", "content": "喜欢观察昆虫", "source": "Manual"}
  ],
  "traits": [
    {"layer": "Base", "trait_label": "温柔", "meaning": "保持温和的说话方式", "not_meaning": null, "trigger": null, "suppress": null, "related": null},
    {"layer": "Accent", "trait_label": "好奇", "meaning": "遇到新鲜事物会追问", "not_meaning": null, "trigger": "谈及昆虫", "suppress": null, "related": null}
  ]
}"#;

/// 拆解成功：直写与拆解条目合并去重后写入，执行情况计数正确。
#[tokio::test]
async fn decompose_success_writes_llm_entries() {
    let llm = Arc::new(MockLlm::with_reply(DECOMPOSE_REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "preset-import-decompose",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    let (path, preset_dir) = write_preset(
        "preset-files-decompose",
        "decompose.toml",
        r#"[persona]
uid = "char-0012"
kind = "char"
name = "拆解人格"

[identity]
assistant_name = "拆解人格"

[blocks]
A_persona = "一个热爱自然的年轻医生。"

[traits]
base = [{ label = "温柔", meaning = "对他人温和" }]

[[facts]]
field = "BasicInfo"
content = "职业是医生"
"#,
    );

    let outcome = engine
        .persona_import(&path, None, false)
        .await
        .expect("导入应成功");

    // 拆解执行情况：已请求 / 已执行 / 无跳过；计数为拆解条目数（含后续被去重项）
    assert!(outcome.decompose.requested);
    assert!(outcome.decompose.executed);
    assert_eq!(outcome.decompose.skipped_reason, None);
    assert_eq!(outcome.decompose.fact_count, 2);
    assert_eq!(outcome.decompose.trait_count, 2);
    assert_eq!(llm.chat_calls(), 1, "拆解应恰调用一次 LLM");

    // 拆解输入含 [blocks] 文本
    let requests = llm.requests();
    assert!(
        requests[0].user_message.contains("一个热爱自然的年轻医生"),
        "拆解输入应含自由文本块"
    );

    // 写入 = 直写 + 拆解（同 label / 同 (field, content) 去重）
    assert_eq!(
        outcome.written,
        ImportWriteStats {
            traits: 2,
            facts: 2,
            examples: 0,
            behavior_rules: 0,
            failed: 0,
        }
    );
    assert_eq!(outcome.preview.will_write.traits.base, 1);
    assert_eq!(outcome.preview.will_write.traits.accent, 1);
    assert_eq!(outcome.preview.will_write.traits.skipped.len(), 1);
    assert_eq!(
        outcome.preview.will_write.traits.skipped[0]
            .label
            .as_deref(),
        Some("温柔")
    );
    assert_eq!(outcome.preview.will_write.facts.skipped.len(), 1);

    let traits = storage
        .list_traits_by_persona("char-0012")
        .await
        .expect("查询应成功");
    assert_eq!(traits.len(), 2);
    let gentle = traits
        .iter()
        .find(|t| t.trait_label == "温柔")
        .expect("应含「温柔」");
    assert_eq!(gentle.meaning, "对他人温和", "直写条目应先于拆解条目落库");
    let curious = traits
        .iter()
        .find(|t| t.trait_label == "好奇")
        .expect("应含「好奇」");
    assert_eq!(curious.layer, TraitLayer::Accent);
    assert_eq!(curious.trigger.as_deref(), Some("谈及昆虫"));

    let facts = storage
        .list_all_facts_by_persona("char-0012")
        .await
        .expect("查询应成功");
    assert_eq!(facts.len(), 2);
    assert!(facts.iter().any(|f| f.content == "喜欢观察昆虫"));

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

/// 拆解失败：导入成功返回、persona 与直写条目照常、配置保留原文。
#[tokio::test]
async fn decompose_failure_degrades_without_losing_persona() {
    let (engine, storage, dir) = engine_with_failing_llm("preset-import-decompose-fail").await;
    let (path, preset_dir) = write_preset(
        "preset-files-decompose-fail",
        "degrade.toml",
        r#"[persona]
uid = "char-0013"
kind = "char"
name = "降级人格"

[identity]
assistant_name = "降级人格"

[blocks]
A_persona = "拆解失败时原文应进配置。"

[traits]
base = [{ label = "坚韧", meaning = "不轻易放弃" }]

[[facts]]
field = "PersonalStatus"
content = "正在准备考试"
"#,
    );

    let outcome = engine
        .persona_import(&path, None, false)
        .await
        .expect("拆解失败不应阻断导入");

    assert!(outcome.decompose.requested);
    assert!(!outcome.decompose.executed);
    assert_eq!(
        outcome.decompose.skipped_reason.as_deref(),
        Some("llm_failed")
    );
    assert_eq!(outcome.decompose.fact_count, 0);
    assert_eq!(outcome.decompose.trait_count, 0);

    // persona 建成、配置保留原文、直写条目全部写入
    let persona = storage
        .get_persona_by_uid("char-0013")
        .await
        .expect("查询应成功")
        .expect("拆解失败仍应建成 persona");
    assert!(
        persona
            .config
            .as_deref()
            .is_some_and(|config| config.contains("A_persona")),
        "配置应保留原文"
    );
    assert_eq!(
        outcome.written,
        ImportWriteStats {
            traits: 1,
            facts: 1,
            examples: 0,
            behavior_rules: 0,
            failed: 0,
        }
    );
    assert_eq!(
        storage
            .list_traits_by_persona("char-0013")
            .await
            .expect("查询应成功")
            .len(),
        1
    );
    assert_eq!(
        storage
            .list_all_facts_by_persona("char-0013")
            .await
            .expect("查询应成功")
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

/// 线上 provider 未确认：跳过拆解且不调用 LLM；persona 照常建成。
#[tokio::test]
async fn decompose_skipped_when_privacy_unconfirmed() {
    let llm = Arc::new(MockLlm::online());
    let (engine, storage, dir) = engine_with_shared_llm(
        "preset-import-privacy",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    let (path, preset_dir) = write_preset(
        "preset-files-privacy",
        "privacy.toml",
        r#"[persona]
uid = "char-0015"
kind = "char"
name = "隐私人格"

[identity]
assistant_name = "隐私人格"

[blocks]
A_persona = "隐私未确认时不应外发。"

[traits]
base = [{ label = "谨慎", meaning = "做事稳妥" }]
"#,
    );

    let outcome = engine
        .persona_import(&path, None, false)
        .await
        .expect("隐私未确认不应阻断导入");

    assert!(outcome.decompose.requested);
    assert!(!outcome.decompose.executed);
    assert_eq!(
        outcome.decompose.skipped_reason.as_deref(),
        Some("privacy_unconfirmed")
    );
    assert_eq!(llm.chat_calls(), 0, "隐私未确认不得调用 LLM");
    assert!(outcome.preview.decompose.enabled);
    assert!(!outcome.preview.decompose.privacy_confirmed);

    let persona = storage
        .get_persona_by_uid("char-0015")
        .await
        .expect("查询应成功")
        .expect("persona 应建成");
    assert!(persona.config.is_some(), "未拆解时原文仍进配置");
    assert_eq!(outcome.written.traits, 1);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// uid 自动生成与 --into 合并
// =========================================================

/// 预设无 uid（无 [persona] 段、kind 缺省）：按同类最大序号 + 1 自动生成。
#[tokio::test]
async fn uid_generated_from_existing_kind_max() {
    let (engine, storage, dir) = engine_with_db("preset-import-uid").await;
    seed_persona(&storage, "char-0003").await;
    let (path, preset_dir) = write_preset(
        "preset-files-uid",
        "auto-persona.toml",
        r#"[identity]
assistant_name = "自动人格"

[traits]
base = [{ label = "安静", meaning = "话不多" }]
"#,
    );

    let outcome = engine
        .persona_import(&path, None, true)
        .await
        .expect("导入应成功");

    assert_eq!(
        outcome.preview.registration.uid.as_deref(),
        Some("char-0004")
    );
    assert_eq!(outcome.preview.registration.mode, PreviewMode::Create);
    assert_eq!(outcome.preview.registration.kind, Some(PersonaKind::Char));
    assert_eq!(
        outcome.preview.registration.kind_source,
        KindSource::Default
    );

    let persona = storage
        .get_persona_by_uid("char-0004")
        .await
        .expect("查询应成功")
        .expect("自动生成 uid 应落库");
    assert_eq!(persona.name, "自动人格");
    assert_eq!(persona.kind, PersonaKind::Char);
    assert_eq!(persona.source, "manual", "导入缺省来源为 manual");
    assert_eq!(persona.seq, 4);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

/// `--into` 合并到已存在 uid：条目写入目标、预设 uid 不创建、提示到位。
#[tokio::test]
async fn merge_into_existing_uid_writes_entries_to_target() {
    let (engine, storage, dir) = engine_with_db("preset-import-into").await;
    seed_persona(&storage, "char-0005").await;
    let (path, preset_dir) = write_preset(
        "preset-files-into",
        "into.toml",
        r#"[persona]
uid = "char-0006"
kind = "char"
name = "黎杋枫"

[identity]
assistant_name = "黎杋枫"

[traits]
base = [{ label = "温和", meaning = "待人温和" }]

[[facts]]
field = "BasicInfo"
content = "插画师"
"#,
    );

    let outcome = engine
        .persona_import(&path, Some("char-0005"), true)
        .await
        .expect("合并导入应成功");

    assert_eq!(
        outcome.preview.registration.uid.as_deref(),
        Some("char-0005")
    );
    assert_eq!(outcome.preview.registration.mode, PreviewMode::Merge);
    assert_eq!(outcome.written.traits, 1);
    assert_eq!(outcome.written.facts, 1);

    // 条目写入目标 uid；预设 uid 未被创建
    assert_eq!(
        storage
            .list_traits_by_persona("char-0005")
            .await
            .expect("查询应成功")
            .len(),
        1
    );
    assert_eq!(
        storage
            .list_all_facts_by_persona("char-0005")
            .await
            .expect("查询应成功")
            .len(),
        1
    );
    assert!(
        storage
            .get_persona_by_uid("char-0006")
            .await
            .expect("查询应成功")
            .is_none()
    );

    // 提示：--into 覆盖预设 uid / kind 不可变更
    let codes: Vec<&str> = outcome
        .preview
        .hints
        .iter()
        .map(|hint| hint.code.as_str())
        .collect();
    assert!(
        codes.contains(&"into_overrides_uid"),
        "应提示 --into 覆盖预设 uid: {codes:?}"
    );
    assert!(
        codes.contains(&"kind_immutable"),
        "应提示 kind 不可变更: {codes:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// 端到端：导入后画像进提示词
// =========================================================

/// 导入（含 description + traits）→ 就绪 → 对话：提示词含描述背景行与 trait 标签。
#[tokio::test]
async fn imported_persona_enters_chat_prompt() {
    const REPLY: &str = "嗯，我在。";
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, _storage, dir) = engine_with_shared_llm(
        "preset-import-e2e",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    let (path, preset_dir) = write_preset(
        "preset-files-e2e",
        "observer.toml",
        r#"[persona]
uid = "char-0014"
kind = "char"
name = "观察者"
description = "冷静沉着的自然观察者"

[identity]
assistant_name = "观察者"

[traits]
base = [{ label = "耐心", meaning = "愿意花时间等待观察" }]
"#,
    );

    let outcome = engine
        .persona_import(&path, None, true)
        .await
        .expect("导入应成功");
    assert_eq!(outcome.written.traits, 1);

    engine.set_state(AppState::Ready);
    let send = engine
        .chat_send(ChatSendRequest {
            message: "你好".to_string(),
            persona: Some("char-0014".to_string()),
            session_id: None,
            conversation_id: None,
            channel: CHANNEL_MCP.to_string(),
        })
        .await
        .expect("导入后应可对话");
    assert_eq!(send.reply, REPLY);

    let requests = llm.requests();
    let prompt = &requests.last().expect("应记录 LLM 请求").system_prompt;
    assert!(
        prompt.contains("背景：冷静沉着的自然观察者"),
        "提示词应含导入的描述背景行"
    );
    assert!(prompt.contains("耐心"), "提示词应含导入的 trait 标签");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// 文件级失败
// =========================================================

/// TOML 语法非法：执行与预览均返回配置错误，且不创建任何 persona。
#[tokio::test]
async fn invalid_preset_file_fails_without_creating_persona() {
    let (engine, storage, dir) = engine_with_db("preset-import-invalid").await;
    let (path, preset_dir) = write_preset(
        "preset-files-invalid",
        "broken.toml",
        "[persona\nname = \"未闭合",
    );

    let err = engine
        .persona_import(&path, None, true)
        .await
        .expect_err("非法 TOML 应返回错误");
    assert_eq!(
        err.category(),
        "config",
        "文件级解析失败应为配置错误: {err}"
    );
    assert!(
        storage
            .list_personas()
            .await
            .expect("查询应成功")
            .is_empty(),
        "失败不应创建 persona"
    );

    let preview_err = engine
        .persona_import_preview(&path, None, true)
        .await
        .expect_err("预览应返回错误");
    assert_eq!(preview_err.category(), "config");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

/// 文件不存在：读取失败返回 IO 错误（消息含文件名）。
#[tokio::test]
async fn missing_file_returns_io_error() {
    let (engine, _storage, dir) = engine_with_db("preset-import-missing").await;
    let preset_dir = temp_dir("preset-files-missing");
    let path = preset_dir.join("nope.toml");

    let err = engine
        .persona_import(&path, None, true)
        .await
        .expect_err("不存在文件应返回错误");
    assert_eq!(err.category(), "io", "读取失败应为 IO 错误: {err}");
    assert!(
        err.to_string().contains("nope.toml"),
        "错误消息应含文件名: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}
