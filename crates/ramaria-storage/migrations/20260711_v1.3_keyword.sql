-- =========================================================
-- v1.3: 关键词倒排索引表 + 集群快照新列
--
-- 用途:
-- 1. keyword_refs: 关键词→业务文档的倒排引用索引，支撑精确匹配检索
-- 2. persona_cluster_snapshots 新增列: 跨版本簇匹配的语义标签
--
-- 设计说明:
-- - keyword_refs.keyword_id 指向 keyword_pool.id（实际为 keyword TEXT PK）
--   此处使用 keyword TEXT 作为 FK 引用，保持与 keyword_pool 主键类型一致
-- - doc_type: 'l1' / 'l2' / 'pool'
-- - 复合索引 idx_keyword_refs_doc 加速按文档查询（正排查）
-- - persona_cluster_snapshots 新增列均为 nullable，不破坏存量数据
-- =========================================================

-- -------------------------------------------------------
-- 1. keyword_refs: 关键词倒排索引表
-- -------------------------------------------------------
CREATE TABLE IF NOT EXISTS keyword_refs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    keyword_id  TEXT    NOT NULL REFERENCES keyword_pool(keyword) ON DELETE CASCADE,
    doc_type    TEXT    NOT NULL,   -- 'l1' / 'l2'
    doc_id      TEXT    NOT NULL,   -- L1: UUID字符串, L2: i64字符串
    persona_uid TEXT    NOT NULL,
    weight      REAL    NOT NULL DEFAULT 1.0,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_keyword_refs_keyword
    ON keyword_refs(keyword_id);

CREATE INDEX IF NOT EXISTS idx_keyword_refs_doc
    ON keyword_refs(doc_type, doc_id);

-- -------------------------------------------------------
-- 2. persona_cluster_snapshots 新增列（跨版本语义标签匹配）
-- -------------------------------------------------------
ALTER TABLE persona_cluster_snapshots ADD COLUMN semantic_label TEXT;
ALTER TABLE persona_cluster_snapshots ADD COLUMN semantic_label_embedding BLOB;
