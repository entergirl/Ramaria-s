-- =========================================================
-- v2.1 会话通道列（channel / external_ref）
-- =========================================================
-- 目的:
-- - sessions 表追加 channel / external_ref，承载外部入口（MCP / 未来社交通道）
--   的会话来源与外部对话标识；存量行取默认值 local / NULL，桌面与 CLI 行为不变。
-- - 联合索引支撑「按 (channel, external_ref) 查活跃会话」这一高频续写定位操作。
--
-- 约定:
-- - 只增不删：不修改既有列、不重建表；新增列均向后兼容。
-- - channel 为开放集合（local / mcp / 后续 telegram / qq 等），不做 CHECK 约束，
--   新通道接入无需再改 schema。

ALTER TABLE sessions ADD COLUMN channel TEXT NOT NULL DEFAULT 'local';

ALTER TABLE sessions ADD COLUMN external_ref TEXT;

CREATE INDEX IF NOT EXISTS idx_sessions_channel_external_ref
    ON sessions (channel, external_ref);
