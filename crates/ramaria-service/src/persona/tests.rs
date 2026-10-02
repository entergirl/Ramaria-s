//! crates/ramaria-service/src/persona/tests.rs - Ramaria 人格用例单元测试
//!
//! 设计特点:
//! - 由 persona 模块以 `#[cfg(test)] mod tests;` 收纳：覆盖 L1 重生成（校验 / 空数据 /
//!   部分失败 / 连续失败早停 / 幂等跳过）、全字段列表与信息更新、文件导入（目录 / 单文件）
//!   与用户人格五组路径
//! - 使用真实 SQLite 临时库（全量 migration）：断言以落库状态与结果条目为准
//! - LLM 交互使用脚本化 mock（含失败 mock）：不连网、不访问 OS keychain
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问网络与真实用户数据。

use super::*;
use crate::test_support::{
    L1_JSON_REPLY, ScriptedLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply,
    engine_with_shared_scripted_llm, seed_persona, seed_session_with_messages,
};
use crate::types::{PersonaFileAction, PersonaUpdateRequest};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{MemoryL1, Persona, PersonaKind};
use ramaria_storage::SqliteStorage;
use std::sync::Arc;
use uuid::Uuid;

/// 造一个"已有目标人格 L1"的会话（带消息；供幂等跳过路径用例）。
async fn seed_session_with_persona_l1(
    storage: &SqliteStorage,
    persona: &str,
    count: usize,
    base_ts: i64,
) -> Uuid {
    let session = seed_session_with_messages(storage, persona, count, base_ts).await;
    let mut l1 = MemoryL1::new(session, "既有摘要".to_string(), None);
    l1.persona_uid = Some(persona.to_string());
    storage
        .save_memory_l1(&l1)
        .await
        .expect("写入既有 L1 应成功");
    session
}

/// 空 UID（含纯空白）：返回业务校验错误。
#[tokio::test]
async fn regenerate_rejects_blank_uid() {
    let (engine, _storage, dir) = engine_with_db("persona-regen-blank-uid").await;

    let err = engine
        .regenerate_persona_l1("   ")
        .await
        .expect_err("空 UID 应返回错误");
    assert_eq!(err.category(), "validation", "应为业务校验错误: {err}");
    assert!(
        err.to_string().contains("人格 UID 不能为空"),
        "错误文案应提示 UID 为空: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 人格不存在：返回业务校验错误（文案含目标 UID）。
#[tokio::test]
async fn regenerate_rejects_missing_persona() {
    let (engine, storage, dir) = engine_with_db("persona-regen-missing").await;
    seed_persona(&storage, "char-0001").await;

    let err = engine
        .regenerate_persona_l1("char-missing")
        .await
        .expect_err("人格不存在应返回错误");
    assert_eq!(err.category(), "validation", "应为业务校验错误: {err}");
    assert!(
        err.to_string().contains("人格不存在: uid=char-missing"),
        "错误文案应含目标 UID: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 该人格无消息：返回零计数与"无需处理"提示，不触达 LLM。
#[tokio::test]
async fn regenerate_returns_noop_without_messages() {
    let (engine, storage, dir) = engine_with_l1_reply("persona-regen-noop", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;

    let outcome = engine
        .regenerate_persona_l1("char-0001")
        .await
        .expect("无消息应正常返回");
    assert_eq!(outcome.total_sessions, 0);
    assert_eq!(outcome.l1_regenerated, 0);
    assert_eq!(outcome.l1_failed, 0);
    assert!(!outcome.early_terminated);
    assert_eq!(outcome.remaining_skipped, 0);
    assert_eq!(outcome.message, "该人格没有关联的导入消息，无需处理。");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 多会话全部成功：逐会话产出绑定人格的 L1，计数与提示为全成功分支。
#[tokio::test]
async fn regenerate_all_sessions_succeed() {
    let (engine, storage, dir) = engine_with_l1_reply("persona-regen-success", L1_JSON_REPLY).await;
    seed_persona(&storage, "char-0001").await;
    let session_a = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
    let session_b = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    let outcome = engine
        .regenerate_persona_l1("char-0001")
        .await
        .expect("重生成应成功");
    assert_eq!(outcome.total_sessions, 2);
    assert_eq!(outcome.l1_regenerated, 2);
    assert_eq!(outcome.l1_failed, 0);
    assert!(!outcome.early_terminated);
    assert_eq!(outcome.remaining_skipped, 0);
    assert_eq!(
        outcome.message,
        "L1 全部重新生成成功 (2/2)。L2/L3 正在后台处理中..."
    );

    for session in [session_a, session_b] {
        let l1_list = storage
            .list_memory_l1(session)
            .await
            .expect("读取 L1 应成功");
        assert_eq!(l1_list.len(), 1, "每个会话应恰有一条 L1");
        assert_eq!(
            l1_list[0].persona_uid.as_deref(),
            Some("char-0001"),
            "L1 应绑定目标人格"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 部分失败未达早停阈值：成功 / 失败计数如实上报，提示为失败分支。
#[tokio::test]
async fn regenerate_reports_partial_failure_without_early_stop() {
    // 脚本队列仅一条有效回复：处理顺序在前的会话成功；其后的会话耗尽队列后
    // 拿到的空回复解析失败，内部重试均失败 → 计入失败，但未达连续失败阈值。
    let llm = Arc::new(ScriptedLlm::replies(&[L1_JSON_REPLY]));
    let (engine, storage, dir) = engine_with_shared_scripted_llm(
        "persona-regen-partial",
        llm,
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, "char-0001").await;
    let ok_session = seed_session_with_messages(&storage, "char-0001", 2, 2_000).await;
    let fail_session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

    let outcome = engine
        .regenerate_persona_l1("char-0001")
        .await
        .expect("部分失败应正常返回");
    assert_eq!(outcome.total_sessions, 2);
    assert_eq!(outcome.l1_regenerated, 1);
    assert_eq!(outcome.l1_failed, 1);
    assert!(!outcome.early_terminated, "失败未达阈值不应提前终止");
    assert_eq!(outcome.remaining_skipped, 0);
    assert_eq!(
        outcome.message,
        "L1 重新生成完成: 成功 1/2, 失败 1。请确认 LLM 模型已连接。L2/L3 正在后台处理中..."
    );

    assert_eq!(
        storage
            .list_memory_l1(ok_session)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "成功会话应产出 L1"
    );
    assert!(
        storage
            .list_memory_l1(fail_session)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "失败会话不应残留 L1"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 连续失败达到阈值：提前终止并报告跳过数量，跳过的会话未被处理。
#[tokio::test]
async fn regenerate_stops_after_consecutive_failures() {
    let (engine, storage, dir) = engine_with_failing_llm("persona-regen-early-stop").await;
    seed_persona(&storage, "char-0001").await;
    let session_1 = seed_session_with_messages(&storage, "char-0001", 3, 4_000).await;
    let session_2 = seed_session_with_messages(&storage, "char-0001", 3, 3_000).await;
    let session_3 = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
    let session_skipped = seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;

    let outcome = engine
        .regenerate_persona_l1("char-0001")
        .await
        .expect("失败路径应返回结果而非错误");
    assert_eq!(outcome.total_sessions, 4);
    assert_eq!(outcome.l1_regenerated, 0);
    assert_eq!(outcome.l1_failed, 3, "连续 3 次失败后应停止");
    assert!(outcome.early_terminated, "应提前终止");
    assert_eq!(outcome.remaining_skipped, 1, "应跳过剩余 1 个会话");
    assert_eq!(
        outcome.message,
        "L1 连续失败 3 次，已提前终止。成功 0/4, 失败 3。请确认 LLM 模型已连接后重试。剩余 1 个 session 未处理。"
    );

    for session in [session_1, session_2, session_3] {
        assert!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "失败的会话不应残留 L1"
        );
    }
    assert!(
        storage
            .list_memory_l1(session_skipped)
            .await
            .expect("读取 L1 应成功")
            .is_empty(),
        "提前终止后跳过的会话不应被处理"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 幂等跳过（已有目标人格 L1）夹在失败之间：不重置连续失败计数，也不计入成功。
#[tokio::test]
async fn regenerate_skip_does_not_reset_failure_streak() {
    let (engine, storage, dir) = engine_with_failing_llm("persona-regen-skip-streak").await;
    seed_persona(&storage, "char-0001").await;
    // 处理顺序按消息时间倒序：失败、跳过、失败、失败、未处理
    let fail_a = seed_session_with_messages(&storage, "char-0001", 3, 5_000).await;
    let skipped = seed_session_with_persona_l1(&storage, "char-0001", 2, 4_000).await;
    let fail_b = seed_session_with_messages(&storage, "char-0001", 3, 3_000).await;
    let fail_c = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
    let fail_unprocessed = seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;

    let outcome = engine
        .regenerate_persona_l1("char-0001")
        .await
        .expect("失败路径应返回结果而非错误");
    assert_eq!(outcome.total_sessions, 5);
    assert_eq!(outcome.l1_regenerated, 0, "跳过不计入成功");
    assert_eq!(
        outcome.l1_failed, 3,
        "跳过不重置连续失败计数：第 4 个失败不应发生"
    );
    assert!(outcome.early_terminated, "应在第 3 次连续失败时提前终止");
    assert_eq!(outcome.remaining_skipped, 1);
    assert_eq!(
        outcome.message,
        "L1 连续失败 3 次，已提前终止。成功 0/5, 失败 3。请确认 LLM 模型已连接后重试。剩余 1 个 session 未处理。"
    );

    assert_eq!(
        storage
            .list_memory_l1(skipped)
            .await
            .expect("读取 L1 应成功")
            .len(),
        1,
        "跳过会话应保留既有 L1"
    );
    for session in [fail_a, fail_b, fail_c, fail_unprocessed] {
        assert!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "失败 / 未处理会话不应有 L1"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 全字段列表：字段逐项映射（含 ref_id / avatar / config / description / 时间戳）。
#[tokio::test]
async fn list_full_maps_all_fields() {
    let (engine, storage, dir) = engine_with_db("persona-full").await;

    let mut persona = Persona::new(
        "char-0001".to_string(),
        "小林".to_string(),
        PersonaKind::Char,
        1,
        "qq".to_string(),
    );
    persona.ref_id = Some("qq-123456".to_string());
    persona.avatar = Some("avatar.png".to_string());
    persona.config = Some("assistant_name = \"小林\"".to_string());
    persona.description = Some("大学同学".to_string());
    storage
        .create_persona(&persona)
        .await
        .expect("插入 persona 应成功");

    let views = engine.persona_list_full().await.expect("列表应成功");
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.uid, "char-0001");
    assert_eq!(view.name, "小林");
    assert_eq!(view.kind, "char");
    assert_eq!(view.source, "qq");
    assert_eq!(view.ref_id.as_deref(), Some("qq-123456"));
    assert_eq!(view.avatar.as_deref(), Some("avatar.png"));
    assert_eq!(view.config.as_deref(), Some("assistant_name = \"小林\""));
    assert_eq!(view.description.as_deref(), Some("大学同学"));
    assert!(view.is_active);
    assert!(view.created_at > 0);
    assert!(view.updated_at >= view.created_at);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 信息更新：提供字段覆盖、未提供字段保持、空描述清空、空 uid / 不存在校验。
#[tokio::test]
async fn update_info_applies_partial_changes() {
    let (engine, storage, dir) = engine_with_db("persona-update").await;
    seed_persona(&storage, "char-0001").await;

    let updated = engine
        .persona_update_info(
            "char-0001",
            PersonaUpdateRequest {
                name: Some("小林".to_string()),
                avatar: Some("avatar.png".to_string()),
                description: Some("大学同学".to_string()),
            },
        )
        .await
        .expect("更新应成功");
    assert_eq!(updated.name, "小林");
    assert_eq!(updated.avatar.as_deref(), Some("avatar.png"));
    assert_eq!(updated.description.as_deref(), Some("大学同学"));

    // 只改描述：名称 / 头像保持旧值
    let updated = engine
        .persona_update_info(
            "char-0001",
            PersonaUpdateRequest {
                name: None,
                avatar: None,
                description: Some("旧同学".to_string()),
            },
        )
        .await
        .expect("更新应成功");
    assert_eq!(updated.name, "小林", "未提供的名称保持旧值");
    assert_eq!(updated.avatar.as_deref(), Some("avatar.png"));
    assert_eq!(updated.description.as_deref(), Some("旧同学"));

    // 空描述：清空（与 None 行为不同）
    let updated = engine
        .persona_update_info(
            "char-0001",
            PersonaUpdateRequest {
                name: None,
                avatar: None,
                description: Some(String::new()),
            },
        )
        .await
        .expect("更新应成功");
    assert_eq!(updated.description.as_deref(), Some(""));

    // 空 uid / 不存在人格：校验错误
    let err = engine
        .persona_update_info("  ", PersonaUpdateRequest::default())
        .await
        .expect_err("空 uid 应报错");
    assert_eq!(err.category(), "validation");
    let err = engine
        .persona_update_info("char-missing", PersonaUpdateRequest::default())
        .await
        .expect_err("人格不存在应报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("人格不存在: uid=char-missing"),
        "错误文案应含目标 uid: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 文件导入：新建 / 已存在更新两条路径、缺失名称回退、uid 过滤、目录缺失报错。
#[tokio::test]
async fn load_from_dir_creates_and_updates() {
    let (engine, storage, dir) = engine_with_db("persona-load").await;
    let personas_dir = dir.join("personas");
    std::fs::create_dir_all(&personas_dir).expect("创建人格目录应成功");

    std::fs::write(
        personas_dir.join("char-0001.toml"),
        "assistant_name = \"小林\"\n[blocks]\nA_persona = \"\"\"\n正文\n\"\"\"\n",
    )
    .expect("写入人格文件应成功");
    // 缺失 assistant_name → 名称回退 uid
    std::fs::write(
        personas_dir.join("char-0002.toml"),
        "[blocks]\nA_persona = \"\"\"\n无名称\n\"\"\"\n",
    )
    .expect("写入人格文件应成功");
    // 非 toml 文件不参与导入
    std::fs::write(personas_dir.join("readme.txt"), "not toml").expect("写入说明文件应成功");

    let outcomes = engine
        .persona_load_from_dir(&personas_dir, None, PersonaLoadMode::CreateOrUpdate)
        .await
        .expect("导入应成功");
    assert_eq!(outcomes.len(), 2, "仅处理 .toml 文件");
    assert_eq!(outcomes[0].uid, "char-0001", "按路径排序处理");
    assert_eq!(outcomes[0].action, PersonaFileAction::Created);
    assert_eq!(outcomes[1].uid, "char-0002");
    assert_eq!(outcomes[1].action, PersonaFileAction::Created);

    let created = storage
        .get_persona_by_uid("char-0001")
        .await
        .expect("查询应成功")
        .expect("应已创建");
    assert_eq!(created.name, "小林");
    assert_eq!(created.kind, PersonaKind::Char);
    assert_eq!(created.source, "file");
    assert!(
        created.config.as_deref().unwrap_or("").contains("正文"),
        "config 应保存文件全文"
    );
    let fallback = storage
        .get_persona_by_uid("char-0002")
        .await
        .expect("查询应成功")
        .expect("应已创建");
    assert_eq!(fallback.name, "char-0002", "缺失 assistant_name 回退 uid");

    // 已存在 → 更新：名称与配置同步，其他字段保持
    storage
        .update_persona("char-0001", "小林", Some("avatar.png"), None, Some("描述"))
        .await
        .expect("预置字段应成功");
    std::fs::write(
        personas_dir.join("char-0001.toml"),
        "assistant_name = \"小林酱\"\n[blocks]\nA_persona = \"\"\"\n新正文\n\"\"\"\n",
    )
    .expect("写入人格文件应成功");

    let outcomes = engine
        .persona_load_from_dir(
            &personas_dir,
            Some("char-0001"),
            PersonaLoadMode::CreateOrUpdate,
        )
        .await
        .expect("导入应成功");
    assert_eq!(outcomes.len(), 1, "uid 过滤只处理目标文件");
    assert_eq!(outcomes[0].uid, "char-0001");
    assert_eq!(outcomes[0].action, PersonaFileAction::Updated);

    let updated = storage
        .get_persona_by_uid("char-0001")
        .await
        .expect("查询应成功")
        .expect("应存在");
    assert_eq!(updated.name, "小林酱");
    assert!(
        updated.config.as_deref().unwrap_or("").contains("新正文"),
        "配置应同步为文件内容"
    );
    assert_eq!(
        updated.avatar.as_deref(),
        Some("avatar.png"),
        "头像等字段保持"
    );
    assert_eq!(updated.description.as_deref(), Some("描述"), "描述保持");

    // 目录不存在：Io 错误（目录解析由调用方负责）
    let err = engine
        .persona_load_from_dir(&dir.join("missing"), None, PersonaLoadMode::CreateOrUpdate)
        .await
        .expect_err("目录不存在应报错");
    assert_eq!(err.category(), "io");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 仅创建缺失模式：已存在记录不被改写（名称 / 配置 / updated_at 均保持），
/// 缺失记录照常创建。
#[tokio::test]
async fn load_from_dir_create_missing_skips_existing() {
    let (engine, storage, dir) = engine_with_db("persona-load-skip").await;
    let personas_dir = dir.join("personas");
    std::fs::create_dir_all(&personas_dir).expect("创建人格目录应成功");

    // 预置已存在记录（名称 / 配置与随后写入的文件内容不同）
    let mut persona = Persona::new(
        "char-0001".to_string(),
        "旧名称".to_string(),
        PersonaKind::Char,
        1,
        "file".to_string(),
    );
    persona.config = Some("assistant_name = \"旧名称\"\n".to_string());
    storage
        .create_persona(&persona)
        .await
        .expect("插入 persona 应成功");
    let before = storage
        .get_persona_by_uid("char-0001")
        .await
        .expect("查询应成功")
        .expect("应存在");

    std::fs::write(
        personas_dir.join("char-0001.toml"),
        "assistant_name = \"新名称\"\n[blocks]\nA_persona = \"\"\"\n新正文\n\"\"\"\n",
    )
    .expect("写入人格文件应成功");
    std::fs::write(
        personas_dir.join("char-0002.toml"),
        "assistant_name = \"小新\"\n",
    )
    .expect("写入人格文件应成功");

    let outcomes = engine
        .persona_load_from_dir(&personas_dir, None, PersonaLoadMode::CreateMissing)
        .await
        .expect("导入应成功");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].uid, "char-0001");
    assert_eq!(
        outcomes[0].action,
        PersonaFileAction::Skipped,
        "已存在记录应跳过"
    );
    assert_eq!(outcomes[1].action, PersonaFileAction::Created);

    // 已存在记录未被改写
    let after = storage
        .get_persona_by_uid("char-0001")
        .await
        .expect("查询应成功")
        .expect("应存在");
    assert_eq!(after.name, "旧名称", "名称不应被改写");
    assert_eq!(
        after.config.as_deref(),
        Some("assistant_name = \"旧名称\"\n"),
        "配置不应被改写"
    );
    assert_eq!(after.updated_at, before.updated_at, "updated_at 不应刷新");

    // 缺失记录正常创建
    let created = storage
        .get_persona_by_uid("char-0002")
        .await
        .expect("查询应成功")
        .expect("应已创建");
    assert_eq!(created.name, "小新");
    assert_eq!(created.source, "file");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 单文件导入（显式 uid）：创建 / 幂等跳过 / 名称兜底 / 读取失败条目。
#[tokio::test]
async fn load_file_uses_explicit_uid_and_name_fallback() {
    let (engine, storage, dir) = engine_with_db("persona-load-file").await;
    let legacy_path = dir.join("persona.toml");

    // 按显式 uid 创建（文件名不参与 uid 解析），名称取文件中的 assistant_name
    std::fs::write(&legacy_path, "assistant_name = \"Ramaria\"\n").expect("写入应成功");
    let outcome = engine
        .persona_load_file(
            &legacy_path,
            "rama-0001",
            "Ramaria",
            PersonaLoadMode::CreateMissing,
        )
        .await;
    assert_eq!(outcome.uid, "rama-0001");
    assert_eq!(outcome.action, PersonaFileAction::Created);
    let created = storage
        .get_persona_by_uid("rama-0001")
        .await
        .expect("查询应成功")
        .expect("应已创建");
    assert_eq!(created.name, "Ramaria");
    assert_eq!(created.kind, PersonaKind::Rama);
    assert_eq!(created.source, "file");

    // 已存在：仅创建缺失模式跳过（不写库）
    let outcome = engine
        .persona_load_file(
            &legacy_path,
            "rama-0001",
            "Ramaria",
            PersonaLoadMode::CreateMissing,
        )
        .await;
    assert_eq!(outcome.action, PersonaFileAction::Skipped);

    // 缺失 assistant_name：名称回退调用方兜底名（与目录导入的 uid 回退不同）
    let unnamed_path = dir.join("persona-unnamed.toml");
    std::fs::write(
        &unnamed_path,
        "[blocks]\nA_persona = \"\"\"\n无名称\n\"\"\"\n",
    )
    .expect("写入应成功");
    let outcome = engine
        .persona_load_file(
            &unnamed_path,
            "char-0009",
            "Ramaria",
            PersonaLoadMode::CreateMissing,
        )
        .await;
    assert_eq!(outcome.action, PersonaFileAction::Created);
    let unnamed = storage
        .get_persona_by_uid("char-0009")
        .await
        .expect("查询应成功")
        .expect("应已创建");
    assert_eq!(unnamed.name, "Ramaria", "缺失名称应回退调用方兜底名");

    // 文件不可读：失败条目（不上抛）
    let outcome = engine
        .persona_load_file(
            &dir.join("missing.toml"),
            "rama-0001",
            "Ramaria",
            PersonaLoadMode::CreateMissing,
        )
        .await;
    assert_eq!(outcome.action, PersonaFileAction::Failed);
    assert!(
        outcome.message.contains("读取文件失败"),
        "失败条目应含原因: {}",
        outcome.message
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 用户人格：首次创建、再次幂等（不重复写库）。
#[tokio::test]
async fn ensure_user_is_idempotent() {
    let (engine, storage, dir) = engine_with_db("persona-user").await;

    assert!(
        engine.persona_ensure_user().await.expect("创建应成功"),
        "首次应创建"
    );
    let user = storage
        .get_persona_by_uid("user-0001")
        .await
        .expect("查询应成功")
        .expect("应存在");
    assert_eq!(user.name, "用户");
    assert_eq!(user.kind, PersonaKind::User);
    assert_eq!(user.source, "system");

    assert!(
        !engine.persona_ensure_user().await.expect("重复应成功"),
        "已存在时不应重复创建"
    );
    assert_eq!(
        storage.list_personas().await.expect("列表应成功").len(),
        1,
        "重复调用不应新增记录"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
