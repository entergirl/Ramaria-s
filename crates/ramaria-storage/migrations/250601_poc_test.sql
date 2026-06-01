-- Phase 0 POC: 验证 sqlx migration 机制可用
-- 正式 schema 在 Phase 1 中设计

CREATE TABLE IF NOT EXISTS poc_test (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL  -- Unix 毫秒时间戳
);
