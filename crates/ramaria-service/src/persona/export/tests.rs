//! crates/ramaria-service/src/persona/export/tests.rs - Ramaria 预设导出用例单元测试
//!
//! 设计特点:
//! - 由 `persona::export` 以 `#[cfg(test)] mod tests;` 收纳：覆盖导出往返（导出 → 再导入一致）/
//!   隐私面（不含证据链、对话示例与会话原文）/ config 重放与降级 / 无关键词规则跳过 / 参数校验
//! - 使用真实 SQLite 临时库造数：断言以导出文本与再导入后的落库值为准
//! - 导出文本写入系统临时目录，用例结束清理
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问网络、OS keychain 与真实用户数据。

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{
    FactSource, FactStatus, Message, MessageRole, MessageSource, Persona, PersonaExample,
    PersonaFact, PersonaKind, PersonalityTrait, ProfileField, TraitSource, TraitStatus,
};

use super::*;
use crate::test_support::{engine_with_db, seed_l1, temp_dir};
use crate::types::ImportWriteStats;

// =========================================================
// 共享夹具
// =========================================================

/// 源人格 uid（导出对象）与再导入目标 uid。
const SOURCE_UID: &str = "char-0021";
const TARGET_UID: &str = "char-0022";

/// 源人格的 config 文本（对话身份 / 自由文本块 / 提示词补丁三段齐备）。
const SOURCE_CONFIG: &str = r#"[identity]
assistant_name = "黎杋枫"
user_name = "用户"
work = "某作品"
era = "唐朝"

[blocks]
A_persona = """角色设定文本。"""
speaking_style = """表达风格文本。"""

[prompt]
role_suffix = "追加角色层。"
style_suffix = "追加表达层。"
"#;

/// 写导出文本到独立临时目录（返回文件路径与目录句柄，调用方负责清理）。
fn write_toml(tag: &str, file_name: &str, content: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(tag);
    let path = dir.join(file_name);
    std::fs::write(&path, content).expect("写入导出文件应成功");
    (path, dir)
}

/// 造源人格的注册列（char-0021：名称 / 头像 / 描述 / config / source）。
async fn seed_source_persona(storage: &Arc<ramaria_storage::SqliteStorage>, config: Option<&str>) {
    let mut persona = Persona::new(
        SOURCE_UID.to_string(),
        "黎杋枫".to_string(),
        PersonaKind::Char,
        21,
        "manual".to_string(),
    );
    persona.avatar = Some("https://example.com/avatar.png".to_string());
    persona.description = Some("温和的医学生".to_string());
    persona.config = config.map(str::to_string);
    storage
        .create_persona(&persona)
        .await
        .expect("创建 persona 应成功");
}

/// 造源人格的 L3 结构化条目：三层 traits / 两条 facts / 一条启用规则 + 一条禁用规则。
async fn seed_source_entries(storage: &Arc<ramaria_storage::SqliteStorage>) {
    let mut gentle = PersonalityTrait::new(
        SOURCE_UID.to_string(),
        TraitLayer::Base,
        "温柔".to_string(),
        "对他人保持温和".to_string(),
        TraitSource::Manual,
        1,
    );
    gentle.status = TraitStatus::Active;
    storage
        .save_trait(&gentle)
        .await
        .expect("写入 trait 应成功");

    let mut slow = PersonalityTrait::new(
        SOURCE_UID.to_string(),
        TraitLayer::Primary,
        "慢热".to_string(),
        "需要时间放松".to_string(),
        TraitSource::Inferred,
        1,
    );
    slow.status = TraitStatus::Active;
    storage.save_trait(&slow).await.expect("写入 trait 应成功");

    let mut insect = PersonalityTrait::new(
        SOURCE_UID.to_string(),
        TraitLayer::Accent,
        "对昆虫着迷".to_string(),
        "谈及昆虫兴致高昂".to_string(),
        TraitSource::Manual,
        1,
    );
    insect.not_meaning = Some("并非对所有生物都着迷".to_string());
    insect.trigger = Some("提及昆虫".to_string());
    insect.suppress = Some("正式场合".to_string());
    insect.related = Some("温柔".to_string());
    storage
        .save_trait(&insect)
        .await
        .expect("写入 trait 应成功");

    // 被覆盖的旧事实（superseded）不应导出
    let mut replaced = PersonaFact::new(
        SOURCE_UID.to_string(),
        ProfileField::BasicInfo,
        "旧版本内容".to_string(),
        FactSource::Event,
    );
    replaced.status = FactStatus::Superseded;
    storage
        .save_fact(&replaced)
        .await
        .expect("写入 fact 应成功");

    let basic = PersonaFact::new(
        SOURCE_UID.to_string(),
        ProfileField::BasicInfo,
        "医学院学生".to_string(),
        FactSource::Manual,
    );
    storage.save_fact(&basic).await.expect("写入 fact 应成功");

    let interest = PersonaFact::new(
        SOURCE_UID.to_string(),
        ProfileField::Interests,
        "喜欢整理手账".to_string(),
        FactSource::Manual,
    );
    storage
        .save_fact(&interest)
        .await
        .expect("写入 fact 应成功");

    let situation = BehaviorSituation {
        keywords: vec!["加班".to_string(), "工作".to_string()],
        ..BehaviorSituation::empty()
    };
    let mut rule = BehaviorRule::new(
        SOURCE_UID.to_string(),
        situation,
        Some("表达关心并建议早点休息".to_string()),
        BehaviorParams {
            emotional_intensity: -0.3,
            ..BehaviorParams::default()
        },
        RuleSource::Manual,
    );
    rule.avoid = vec!["说教".to_string()];
    storage
        .save_behavior_rule(&rule)
        .await
        .expect("写入规则应成功");

    let mut disabled = BehaviorRule::new(
        SOURCE_UID.to_string(),
        BehaviorSituation {
            keywords: vec!["电影".to_string()],
            ..BehaviorSituation::empty()
        },
        Some("禁用规则".to_string()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    disabled.enabled = false;
    storage
        .save_behavior_rule(&disabled)
        .await
        .expect("写入规则应成功");
}

// =========================================================
// 导出往返（导出 → 再导入一致）
// =========================================================

/// 完整画像（含 config 重放）导出 → 再导入到新 uid：注册列 / 四类条目 / config 文本逐项一致。
#[tokio::test]
async fn export_roundtrip_reproduces_profile() {
    let (engine, storage, dir) = engine_with_db("preset-export-roundtrip").await;
    seed_source_persona(&storage, Some(SOURCE_CONFIG)).await;
    seed_source_entries(&storage).await;

    let outcome = engine.persona_export(SOURCE_UID).await.expect("导出应成功");

    // 计数：traits 3（含 Inferred）/ facts 2（superseded 不导出）/ 启用规则 1（禁用不导出）
    assert_eq!(outcome.uid, SOURCE_UID);
    assert_eq!(outcome.name, "黎杋枫");
    assert_eq!(
        outcome.stats,
        PersonaExportStats {
            traits: 3,
            facts: 2,
            behavior_rules: 1,
            blocks: 2,
            prompt_patch: true,
        }
    );
    assert!(outcome.hints.is_empty(), "正常导出不应有降级提示");

    // 导出文本含关键内容（注册列 / 三层 / 事实 / 规则 / 补丁）
    for needle in [
        "uid = \"char-0021\"",
        "kind = \"char\"",
        "label = \"温柔\"",
        "label = \"慢热\"",
        "label = \"对昆虫着迷\"",
        "content = \"医学院学生\"",
        "\"加班\"",
        "\"工作\"",
        "role_suffix",
        "A_persona",
    ] {
        assert!(
            outcome.toml.contains(needle),
            "导出文本应包含 {needle}：\n{}",
            outcome.toml
        );
    }

    // 解析层面：导出文本可被预设解析器原样消费（身份 / 自由文本块 / 补丁重放）
    let parsed = ramaria_memory::preset::parse_preset(
        outcome.toml.as_bytes(),
        &PresetParseContext {
            file_stem: None,
            default_source: "manual".to_string(),
        },
    )
    .expect("导出文本应可被预设解析器消费");
    assert_eq!(parsed.identity.assistant_name, "黎杋枫");
    assert_eq!(parsed.identity.user_name.as_deref(), Some("用户"));
    assert_eq!(parsed.identity.work.as_deref(), Some("某作品"));
    assert_eq!(parsed.identity.era.as_deref(), Some("唐朝"));
    assert_eq!(parsed.blocks.len(), 2);
    assert_eq!(parsed.prompt.role_suffix.as_deref(), Some("追加角色层。"));
    assert_eq!(parsed.prompt.style_suffix.as_deref(), Some("追加表达层。"));
    assert_eq!(
        parsed
            .facts
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>(),
        vec!["医学院学生", "喜欢整理手账"]
    );

    // ---- 再导入：目标 uid 不存在 → 新建，逐项比对 ----
    let (path, preset_dir) = write_toml(
        "preset-export-roundtrip-files",
        "char-0021.toml",
        &outcome.toml,
    );
    let imported = engine
        .persona_import(&path, Some(TARGET_UID), true)
        .await
        .expect("再导入应成功");
    assert_eq!(
        imported.written,
        ImportWriteStats {
            traits: 3,
            facts: 2,
            examples: 0,
            behavior_rules: 1,
            failed: 0,
        }
    );

    let source = storage
        .get_persona_by_uid(SOURCE_UID)
        .await
        .expect("查询应成功")
        .expect("源人格应存在");
    let target = storage
        .get_persona_by_uid(TARGET_UID)
        .await
        .expect("查询应成功")
        .expect("目标人格应存在");
    assert_eq!(target.name, source.name);
    assert_eq!(target.kind, source.kind);
    assert_eq!(target.avatar, source.avatar);
    assert_eq!(target.description, source.description);
    assert_eq!(target.source, source.source);
    assert_eq!(
        target.config.as_deref(),
        Some(outcome.toml.as_str()),
        "TOML 来源应存导出文本原文"
    );

    // traits：三层逐条一致（source 不作比对——预设导入统一落 Manual）
    let source_traits = storage
        .list_traits_by_persona(SOURCE_UID)
        .await
        .expect("查询应成功");
    let target_traits = storage
        .list_traits_by_persona(TARGET_UID)
        .await
        .expect("查询应成功");
    let key = |t: &PersonalityTrait| (t.layer, t.trait_label.clone());
    let mut source_keys: Vec<_> = source_traits.iter().map(key).collect();
    let mut target_keys: Vec<_> = target_traits.iter().map(key).collect();
    source_keys.sort_by_key(|k| format!("{:?}{}", k.0, k.1));
    target_keys.sort_by_key(|k| format!("{:?}{}", k.0, k.1));
    assert_eq!(source_keys, target_keys);

    let source_insect = source_traits
        .iter()
        .find(|t| t.trait_label == "对昆虫着迷")
        .expect("应含「对昆虫着迷」");
    let target_insect = target_traits
        .iter()
        .find(|t| t.trait_label == "对昆虫着迷")
        .expect("应含「对昆虫着迷」");
    assert_eq!(target_insect.meaning, source_insect.meaning);
    assert_eq!(target_insect.not_meaning, source_insect.not_meaning);
    assert_eq!(target_insect.trigger, source_insect.trigger);
    assert_eq!(target_insect.suppress, source_insect.suppress);
    assert_eq!(target_insect.related, source_insect.related);

    // facts：active 两条一致
    let target_facts = storage
        .list_active_facts_by_persona(TARGET_UID)
        .await
        .expect("查询应成功");
    let target_fact_pairs: Vec<_> = target_facts
        .iter()
        .map(|f| (f.field, f.content.clone()))
        .collect();
    assert_eq!(target_fact_pairs.len(), 2, "active 事实应为两条");
    assert!(target_fact_pairs.contains(&(ProfileField::BasicInfo, "医学院学生".to_string())));
    assert!(target_fact_pairs.contains(&(ProfileField::Interests, "喜欢整理手账".to_string())));

    // behavior_rules：启用规则一条一致（禁用规则未导出）
    let target_rules = storage
        .list_behavior_rules_by_persona(TARGET_UID)
        .await
        .expect("查询应成功");
    assert_eq!(target_rules.len(), 1);
    assert_eq!(target_rules[0].situation.keywords, vec!["加班", "工作"]);
    assert_eq!(
        target_rules[0].reaction.as_deref(),
        Some("表达关心并建议早点休息")
    );
    assert_eq!(target_rules[0].avoid, vec!["说教"]);
    assert_eq!(target_rules[0].params.emotional_intensity, -0.3);
    assert_eq!(target_rules[0].source, RuleSource::Manual);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&preset_dir);
}

// =========================================================
// 隐私面
// =========================================================

/// 会话原文（L0 消息 / L1 摘要）与对话示例不进导出文本（画像级导出）。
#[tokio::test]
async fn export_excludes_raw_text_and_examples() {
    let (engine, storage, dir) = engine_with_db("preset-export-privacy").await;
    seed_source_persona(&storage, Some(SOURCE_CONFIG)).await;
    seed_source_entries(&storage).await;

    const MSG_SENTINEL: &str = "SENTINEL_对话原文_7f3a";
    const L1_SENTINEL: &str = "SENTINEL_摘要原文_9c21";
    const EXAMPLE_SENTINEL: &str = "SENTINEL_示例原文_4b8e";

    // 会话 + 消息（含哨兵原文）
    let session = storage
        .create_session(Some(SOURCE_UID))
        .await
        .expect("创建会话应成功");
    let message = Message::new(
        session.id,
        MessageRole::User,
        MSG_SENTINEL.to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(SOURCE_UID.to_string()));
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");

    // L1 摘要 + 对话示例（各含哨兵）
    seed_l1(
        &storage,
        SOURCE_UID,
        L1_SENTINEL,
        Some("加班"),
        1_700_000_000_000,
    )
    .await;
    storage
        .save_example(&PersonaExample::new(
            SOURCE_UID.to_string(),
            EXAMPLE_SENTINEL.to_string(),
            EXAMPLE_SENTINEL.to_string(),
        ))
        .await
        .expect("写入示例应成功");

    let outcome = engine.persona_export(SOURCE_UID).await.expect("导出应成功");

    for sentinel in [MSG_SENTINEL, L1_SENTINEL, EXAMPLE_SENTINEL] {
        assert!(
            !outcome.toml.contains(sentinel),
            "导出文本不应包含 {sentinel}：\n{}",
            outcome.toml
        );
    }
    assert!(!outcome.toml.contains("[[examples]]"), "不导出对话示例段");
    assert_eq!(outcome.stats.facts, 2);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 降级与边界
// =========================================================

/// config 缺失 / 不可解析：降级为注册名兜底身份 + 无自由文本块 + 无补丁，并附提示。
#[tokio::test]
async fn export_degrades_when_config_undecodable() {
    let (engine, storage, dir) = engine_with_db("preset-export-degrade").await;
    seed_source_persona(&storage, None).await;
    seed_source_entries(&storage).await;

    let outcome = engine.persona_export(SOURCE_UID).await.expect("导出应成功");
    assert_eq!(outcome.stats.blocks, 0);
    assert!(!outcome.stats.prompt_patch);
    assert_eq!(outcome.hints, vec![CONFIG_UNDECODABLE_HINT.to_string()]);
    assert!(
        outcome.toml.contains("assistant_name = \"黎杋枫\""),
        "身份应回落注册名：\n{}",
        outcome.toml
    );
    assert!(!outcome.toml.contains("[blocks]"), "无 blocks 段");

    // 非预设文本同样降级（不因 config 非法而失败）
    let (engine2, storage2, dir2) = engine_with_db("preset-export-degrade-garbage").await;
    seed_source_persona(&storage2, Some("这不是合法的 persona 配置")).await;
    let outcome2 = engine2
        .persona_export(SOURCE_UID)
        .await
        .expect("导出应成功");
    assert_eq!(outcome2.hints.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// 无关键词的启用规则被跳过并计数（预设格式要求 keywords 非空）。
#[tokio::test]
async fn export_skips_rules_without_keywords() {
    let (engine, storage, dir) = engine_with_db("preset-export-rule-skip").await;
    seed_source_persona(&storage, Some(SOURCE_CONFIG)).await;

    let rule = BehaviorRule::new(
        SOURCE_UID.to_string(),
        BehaviorSituation::empty(),
        Some("无关键词规则".to_string()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    storage
        .save_behavior_rule(&rule)
        .await
        .expect("写入规则应成功");

    let outcome = engine.persona_export(SOURCE_UID).await.expect("导出应成功");
    assert_eq!(outcome.stats.behavior_rules, 0);
    assert_eq!(outcome.hints.len(), 1);
    assert!(
        outcome.hints[0].contains("无关键词"),
        "提示应说明跳过原因：{:?}",
        outcome.hints
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// uid 空白 / 人格不存在：返回 `Validation` 错误（不建文件、不写库）。
#[tokio::test]
async fn export_validates_uid_and_existence() {
    let (engine, storage, dir) = engine_with_db("preset-export-validation").await;

    let blank = engine
        .persona_export("   ")
        .await
        .expect_err("空白 uid 应报错");
    assert_eq!(blank.category(), "validation");

    let missing = engine
        .persona_export("char-9999")
        .await
        .expect_err("人格不存在应报错");
    assert_eq!(missing.category(), "validation");
    assert!(missing.to_string().contains("人格不存在"));

    assert!(
        storage
            .list_personas()
            .await
            .expect("查询应成功")
            .is_empty(),
        "校验失败不应创建任何记录"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
