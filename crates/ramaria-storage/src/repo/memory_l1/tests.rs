//! crates/ramaria-storage/src/repo/memory_l1/tests.rs - L1 摘要存取单元测试
//!
//! 设计特点:
//! - 覆盖 evidence_notes 结构化槽位往返（get / list_by_session 两条读取路径）
//! - 覆盖 None 与空数组的语义区分，以及 continuation 的读写一致性
//! - 覆盖无主 L1 归属 persona 的幂等语义
//! - 使用内存 SQLite 真库，migration 产物直接参与读取

use super::*;
use crate::database;

/// 构造一条带完整槽位的 MemoryL1（text/time/who/cause 全填）。
fn make_l1_with_slots(session_id: Uuid) -> MemoryL1 {
    MemoryL1 {
        id: Uuid::new_v4(),
        session_id,
        summary: "用户讨论项目延期安排".into(),
        keywords: Some("项目,延期,排期".into()),
        time_period: Some("下午".into()),
        atmosphere: Some("紧张".into()),
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: 1_700_000_000_000,
        occurred_at: None,
        last_accessed_at: None,
        persona_uid: Some("char-0001".into()),
        context_json: None,
        situation_strength: Some(4),
        evidence_notes: Some(vec![EvidenceNote {
            text: "用户提到项目延期到月底".into(),
            time: Some("上周三".into()),
            who: Some("用户".into()),
            cause: Some("需求变更频繁".into()),
        }]),
        continuation: None,
    }
}

/// 创建测试 fixture：persona + session（满足 memory_l1 的外键约束）。
///
/// 说明: init_test_pool 每次创建全新内存库，必须显式插入引用数据，
/// 否则 save 触发 FOREIGN KEY constraint failed。
async fn setup_fixture(pool: &SqlitePool) -> Uuid {
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0)",
    )
    .execute(pool)
    .await
    .expect("插入 persona fixture 应成功");
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .expect("插入 session fixture 应成功");
    session_id
}

/// 新格式往返：save → get，结构化槽位完整保留（验收 1）。
#[tokio::test]
async fn evidence_notes_structured_roundtrip_via_get() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;
    let l1 = make_l1_with_slots(session_id);

    save(&pool, &l1).await.expect("save 应成功");

    let got = get(&pool, l1.id)
        .await
        .expect("get 应成功")
        .expect("应存在");
    assert_eq!(got.summary, l1.summary);
    let notes = got.evidence_notes.expect("evidence_notes 不应为 None");
    assert_eq!(notes.len(), 1, "1 条结构化线索应往返保留");
    assert_eq!(notes[0].text, "用户提到项目延期到月底");
    assert_eq!(notes[0].time.as_deref(), Some("上周三"));
    assert_eq!(notes[0].who.as_deref(), Some("用户"));
    assert_eq!(notes[0].cause.as_deref(), Some("需求变更频繁"));
}

/// 新格式往返：save → list_by_session，多条含/不含槽位的线索均完整（验收 1）。
#[tokio::test]
async fn evidence_notes_structured_roundtrip_via_list_by_session() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;
    let mut l1 = make_l1_with_slots(session_id);
    // 追加一条仅 text 的线索（可选槽位缺省）
    l1.evidence_notes
        .as_mut()
        .unwrap()
        .push(EvidenceNote::new("用户表示新方案还在评估中"));

    save(&pool, &l1).await.expect("save 应成功");

    let list = list_by_session(&pool, session_id)
        .await
        .expect("list_by_session 应成功");
    assert_eq!(list.len(), 1);
    let notes = list[0].evidence_notes.as_ref().unwrap();
    assert_eq!(notes.len(), 2);
    // 第一条：完整槽位
    assert_eq!(notes[0].cause.as_deref(), Some("需求变更频繁"));
    // 第二条：仅 text，可选槽位为 None
    assert_eq!(notes[1].text, "用户表示新方案还在评估中");
    assert!(notes[1].time.is_none());
    assert!(notes[1].who.is_none());
    assert!(notes[1].cause.is_none());
}

/// 空值往返：evidence_notes 为 None → save → get 仍为 None。
#[tokio::test]
async fn evidence_notes_none_roundtrip() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;
    let mut l1 = make_l1_with_slots(session_id);
    l1.evidence_notes = None;

    save(&pool, &l1).await.expect("save 应成功");

    let got = get(&pool, l1.id).await.unwrap().unwrap();
    assert!(got.evidence_notes.is_none(), "None 应保持 None（DB NULL）");
}

/// 空数组往返：evidence_notes 为 Some(vec![]) → save → get 读回空数组
/// （存储为 `[]` 而非 NULL，与 None 语义区分；验收 3）。
#[tokio::test]
async fn evidence_notes_empty_array_roundtrip() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;
    let mut l1 = make_l1_with_slots(session_id);
    l1.evidence_notes = Some(vec![]);

    save(&pool, &l1).await.expect("save 应成功");

    let got = get(&pool, l1.id).await.unwrap().unwrap();
    let notes = got.evidence_notes.expect("空数组应读回 Some(vec![])");
    assert!(notes.is_empty(), "空数组应往返为空数组");
}

/// continuation 往返：Some("延续") → save → get 读回；None → 读回 None
/// （v1.5 B2 存储层验收）。
#[tokio::test]
async fn continuation_roundtrip() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;

    // Some("延续") 往返
    let mut l1 = make_l1_with_slots(session_id);
    l1.continuation = Some("延续".to_string());
    save(&pool, &l1).await.expect("save 应成功");
    let got = get(&pool, l1.id).await.unwrap().unwrap();
    assert_eq!(got.continuation.as_deref(), Some("延续"));

    // 非法枚举值（防御：若上游未校验，存储层原样保存）——仅验证读写一致性
    let mut l1b = make_l1_with_slots(session_id);
    l1b.id = Uuid::new_v4();
    l1b.continuation = Some("无关".to_string());
    save(&pool, &l1b).await.expect("save 应成功");
    let got_b = get(&pool, l1b.id).await.unwrap().unwrap();
    assert_eq!(got_b.continuation.as_deref(), Some("无关"));

    // None（首块/独立摘要路径）往返
    let mut l1c = make_l1_with_slots(session_id);
    l1c.id = Uuid::new_v4();
    l1c.continuation = None;
    save(&pool, &l1c).await.expect("save 应成功");
    let got_c = get(&pool, l1c.id).await.unwrap().unwrap();
    assert!(got_c.continuation.is_none(), "None 应往返为 None");
}

/// 多槽位 JSON 序列化不含空槽位（skip_serializing_if）：存储紧凑且迁移幂等友好。
#[tokio::test]
async fn evidence_notes_serialized_json_omits_empty_slots() {
    let note = EvidenceNote::new("用户提到周末加班");
    let json = serde_json::to_string(&vec![note]).unwrap();
    assert!(
        !json.contains("time") && !json.contains("who") && !json.contains("cause"),
        "空槽位不应出现在存储 JSON 中: {json}"
    );
    assert!(json.contains("用户提到周末加班"));
}

// =========================================================
// assign_persona_uid：无主 L1 归属（数据断层修复链路）
// =========================================================

/// 构造一条无主 L1（persona_uid = None），复用 make_l1_with_slots 的槽位。
fn make_unbound_l1(session_id: Uuid) -> MemoryL1 {
    let mut l1 = make_l1_with_slots(session_id);
    l1.persona_uid = None;
    l1
}

/// 基本归属：无主未吸收 L1 被归属到目标 persona，重复归属幂等。
#[tokio::test]
async fn assign_persona_uid_attributes_unbound() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;

    // 3 条无主 L1（persona_uid=NULL）
    let l1a = make_unbound_l1(session_id);
    let l1b = make_unbound_l1(session_id);
    let l1c = make_unbound_l1(session_id);
    for l1 in [&l1a, &l1b, &l1c] {
        save(&pool, l1).await.expect("save 应成功");
    }

    // 归属其中 2 条到 char-0001
    let assigned = assign_persona_uid(&pool, &[l1a.id, l1b.id], "char-0001")
        .await
        .expect("归属应成功");
    assert_eq!(assigned, 2, "应归属 2 条无主 L1");

    // 校验：目标 persona 读回 2 条；无主通道读回 1 条
    let bound = list_unabsorbed(&pool, "char-0001")
        .await
        .expect("查询应成功");
    assert_eq!(bound.len(), 2, "char-0001 应读到 2 条");
    assert!(
        bound
            .iter()
            .all(|l| l.persona_uid.as_deref() == Some("char-0001"))
    );

    let unbound = list_unabsorbed_unbound(&pool).await.expect("查询应成功");
    assert_eq!(unbound.len(), 1, "无主通道应剩 1 条");
    assert_eq!(unbound[0].id, l1c.id, "剩余应为未归属的 l1c");

    // 幂等：重复归属同一批 → 不再更新（已归属）
    let assigned2 = assign_persona_uid(&pool, &[l1a.id, l1b.id], "char-0001")
        .await
        .expect("重复归属应成功");
    assert_eq!(assigned2, 0, "重复归属应为 0（幂等）");
}

/// 边界：不覆盖既有归属；不触碰已吸收记录；空 id 列表返回 0。
#[tokio::test]
async fn assign_persona_uid_respects_boundaries() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;

    // 1) 已有归属的 L1：尝试重新归属到其他 persona → 不被覆盖
    let l1_bound = make_l1_with_slots(session_id); // persona_uid=char-0001
    save(&pool, &l1_bound).await.expect("save 应成功");
    let assigned = assign_persona_uid(&pool, &[l1_bound.id], "char-9999")
        .await
        .expect("归属应成功");
    assert_eq!(assigned, 0, "既有归属不应被覆盖");
    let got = get(&pool, l1_bound.id).await.unwrap().unwrap();
    assert_eq!(got.persona_uid.as_deref(), Some("char-0001"), "原归属保持");

    // 2) 已吸收的无主 L1：不触碰（absorbed=1）
    let mut l1_absorbed = make_unbound_l1(session_id);
    l1_absorbed.id = Uuid::new_v4();
    l1_absorbed.absorbed = true;
    save(&pool, &l1_absorbed).await.expect("save 应成功");
    let assigned2 = assign_persona_uid(&pool, &[l1_absorbed.id], "char-0001")
        .await
        .expect("归属应成功");
    assert_eq!(assigned2, 0, "已吸收记录不应被触碰");
    let got2 = get(&pool, l1_absorbed.id).await.unwrap().unwrap();
    assert!(got2.persona_uid.is_none(), "已吸收记录保持无主");

    // 3) 空 id 列表 → 返回 0
    let assigned3 = assign_persona_uid(&pool, &[], "char-0001")
        .await
        .expect("空列表应成功");
    assert_eq!(assigned3, 0, "空列表应返回 0");
}

/// 大批量归属分批（>100 条）：全部归属成功（SQLite 参数上限保护）。
#[tokio::test]
async fn assign_persona_uid_batch_chunking() {
    let pool = database::init_test_pool().await.unwrap();
    let session_id = setup_fixture(&pool).await;

    // 250 条无主 L1（跨 3 批：100+100+50）
    let mut ids = Vec::with_capacity(250);
    for _ in 0..250 {
        let l1 = make_unbound_l1(session_id);
        ids.push(l1.id);
        save(&pool, &l1).await.expect("save 应成功");
    }

    let assigned = assign_persona_uid(&pool, &ids, "char-0001")
        .await
        .expect("分批归属应成功");
    assert_eq!(assigned, 250, "全部 250 条应归属成功");

    let bound = list_unabsorbed(&pool, "char-0001")
        .await
        .expect("查询应成功");
    assert_eq!(bound.len(), 250, "归属后目标 persona 应有 250 条");
}
