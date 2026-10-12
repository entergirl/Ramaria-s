//! crates/ramaria-service/src/import/identity_tests.rs - Ramaria 导入身份与 L1 分发口径用例
//!
//! 设计特点:
//! - 覆盖群聊 L1 参与者分发的显式身份口径：恒含导出者（User 角色发言者）与块内他人
//! - 断言以库内 L1 行为准：行归属、吸收按人独立、幂等重跑与进度事件序列
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::tests::{RecordingSink, import_engine, import_request, qq_group_export_json};
use super::*;
use ramaria_core::traits::StoreCrud;

/// 群聊 L1 分发：每会话一次生成 × 参与者（含导出者）各一行；吸收按人独立；幂等。
#[tokio::test]
async fn generate_l1_group_fanout_dispatches_rows() {
    let (engine, _pool, storage, dir) = import_engine("import-group-l1").await;
    let file_path = dir.join("group.json");
    std::fs::write(&file_path, qq_group_export_json()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect("群聊 L0 导入应成功");
    assert_eq!(outcome.chat_type, "group");

    let make_plan = || ImportL1Plan {
        targets: Vec::new(),
        l1_prefix: None,
        cascade: false,
        throttle_ms: 0,
        group_fanout: true,
        fanout_include_self: true,
    };

    let sink = RecordingSink::new();
    let l1 = engine
        .generate_import_l1(&outcome.session_ids, make_plan(), Some(&sink))
        .await
        .expect("群聊 L1 批量生成应成功");

    assert_eq!(l1.l1_total, 3, "群聊分发每会话一次生成");
    assert_eq!(l1.l1_success, 3);
    assert_eq!(l1.l1_failed, 0);
    assert_eq!(l1.l1_skipped, 0);
    assert_eq!(l1.l1_processed, 3);

    // 起始进度与逐 session 进度：总量 = session 数，文案为参与者分发
    let events = sink.events();
    assert_eq!(events.len(), 4, "起始进度 + 每 session 一条");
    assert_eq!(events[0].current, 0);
    assert_eq!(events[0].total, 3);
    assert!(
        events[0].message.contains("参与者分发"),
        "起始文案应标注参与者分发: {}",
        events[0].message
    );
    assert!(
        events
            .last()
            .expect("应有进度事件")
            .message
            .contains("参与者分发"),
        "完成文案应标注参与者分发"
    );

    // 行结构：session 1（导出者 + 两位他人）→ 3 行；session 2（纯导出者）→ 1 行归属导出者；session 3（导出者 + 一位他人）→ 2 行
    let rows_s1 = storage
        .list_memory_l1(outcome.session_ids[0])
        .await
        .expect("读取 L1 应成功");
    assert_eq!(rows_s1.len(), 3, "导出者与块内两位他人 → 每人一行");
    for uid in ["user-10001", "char-90002", "char-90003"] {
        assert!(
            rows_s1
                .iter()
                .any(|row| row.persona_uid.as_deref() == Some(uid)),
            "session 1 应有 {uid} 行"
        );
    }

    let rows_s2 = storage
        .list_memory_l1(outcome.session_ids[1])
        .await
        .expect("读取 L1 应成功");
    assert_eq!(rows_s2.len(), 1, "纯导出者会话 → 1 行");
    assert_eq!(
        rows_s2[0].persona_uid.as_deref(),
        Some("user-10001"),
        "纯自我块分发行归属导出者（不再有无主行）"
    );

    let rows_s3 = storage
        .list_memory_l1(outcome.session_ids[2])
        .await
        .expect("读取 L1 应成功");
    assert_eq!(rows_s3.len(), 2, "导出者与一位他人 → 各一行");
    assert!(
        rows_s3
            .iter()
            .any(|row| row.persona_uid.as_deref() == Some("user-10001")),
        "session 3 应有导出者行"
    );
    assert!(
        rows_s3
            .iter()
            .any(|row| row.persona_uid.as_deref() == Some("char-90002")),
        "session 3 应有 char-90002 行"
    );

    // absorbed 独立：吸收 char-90002 的一行不影响 char-90003 的未吸收集合
    let row_a = rows_s1
        .iter()
        .find(|row| row.persona_uid.as_deref() == Some("char-90002"))
        .expect("session 1 应有 char-90002 行");
    assert!(
        rows_s1
            .iter()
            .any(|row| row.persona_uid.as_deref() == Some("char-90003")),
        "session 1 应有 char-90003 行"
    );
    storage
        .mark_l1_absorbed(&[row_a.id])
        .await
        .expect("标记吸收应成功");

    let unabsorbed_b = storage
        .list_unabsorbed_l1("char-90003")
        .await
        .expect("查询未吸收 L1 应成功");
    assert_eq!(unabsorbed_b.len(), 1, "char-90003 未吸收行不受吸收影响");
    assert!(
        unabsorbed_b
            .iter()
            .all(|row| row.persona_uid.as_deref() == Some("char-90003")),
        "按人独立返回本 persona 的行"
    );

    // L2 按人独立（数据面）：char-90002 剩余 session 3 一行未吸收
    let unabsorbed_a = storage
        .list_unabsorbed_l1("char-90002")
        .await
        .expect("查询未吸收 L1 应成功");
    assert_eq!(unabsorbed_a.len(), 1, "char-90002 剩余一行（session 3）");
    assert!(
        !unabsorbed_a.iter().any(|row| row.id == row_a.id),
        "已吸收行不应再返回"
    );

    // 幂等：再次生成不新增行数（已分发会话跳过重生成，纯自我会话已有归属行跳过）
    engine
        .generate_import_l1(&outcome.session_ids, make_plan(), None)
        .await
        .expect("重复生成应成功");
    let mut total_rows = 0usize;
    for session_id in &outcome.session_ids {
        total_rows += storage
            .list_memory_l1(*session_id)
            .await
            .expect("读取 L1 应成功")
            .len();
    }
    assert_eq!(total_rows, 6, "重复生成不新增 L1 行数");

    let _ = std::fs::remove_dir_all(&dir);
}
