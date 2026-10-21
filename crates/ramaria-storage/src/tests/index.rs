//! crates/ramaria-storage/src/tests/index.rs - 索引版本与语料戳存储测试
//!
//! 设计特点:
//! - 覆盖 BM25 分词版本辅助（缺省 / 往返 / 非法值回退）
//! - 覆盖索引语料戳随写入变化的统计口径

use super::*;

/// BM25 分词版本辅助：缺失默认旧版 1、读写往返、settings 键真实落库。
#[tokio::test]
async fn bm25_index_version_helper() {
    let storage = setup().await;

    // 键缺失 → 默认旧版本 1（None/缺失视为 1）
    assert_eq!(storage.get_bm25_index_version().await.unwrap(), 1);
    assert_eq!(
        storage.get_bm25_index_version().await.unwrap(),
        BM25_INDEX_VERSION_LEGACY
    );

    // 写 2 → 读 2 往返，且底层 settings 表键真实写入
    storage
        .set_bm25_index_version(BM25_INDEX_VERSION_CURRENT)
        .await
        .unwrap();
    assert_eq!(
        storage.get_bm25_index_version().await.unwrap(),
        BM25_INDEX_VERSION_CURRENT
    );
    assert_eq!(
        storage
            .get_setting(SETTING_BM25_INDEX_VERSION)
            .await
            .unwrap()
            .as_deref(),
        Some("2")
    );

    // 非法值回退旧版（防御）
    storage
        .set_setting(SETTING_BM25_INDEX_VERSION, "not-a-number")
        .await
        .unwrap();
    assert_eq!(
        storage.get_bm25_index_version().await.unwrap(),
        BM25_INDEX_VERSION_LEGACY
    );
}

/// 索引语料戳：空库为零值，写入 L1 后条数与时间戳同步变化。
#[tokio::test]
async fn index_corpus_stamp_tracks_writes() {
    let storage = setup().await;

    // 空库：全部为 0（默认值语义，无 NULL 泄漏）
    let empty = storage
        .index_corpus_stamp()
        .await
        .unwrap()
        .expect("SQLite 后端应提供语料统计");
    assert_eq!(empty, IndexCorpusStamp::default());

    // 写入 L1 后：条数增加、最新写入时间非 0（跨进程刷新检测的触发源）
    let session = storage.create_session(None).await.unwrap();
    let l1 = MemoryL1::new(session.id, "用户提到最近在准备考试".to_string(), None);
    storage.save_memory_l1(&l1).await.unwrap();

    let stamp = storage.index_corpus_stamp().await.unwrap().unwrap();
    assert_eq!(stamp.l1_count, 1);
    assert!(stamp.l1_max_created_at > 0, "有写入时最新时间戳应非 0");
    assert_ne!(stamp, empty, "语料变化必须体现在统计戳上");
    // 其余语料未写入：保持 0
    assert_eq!(stamp.event_count, 0);
    assert_eq!(stamp.utt_count, 0);
    assert_eq!(stamp.persona_count, 0);
}
