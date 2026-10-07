-- =========================================================
-- 发送者身份（messages 两列 + session_members 成员表）
-- =========================================================
-- 目的:
-- - messages 追加发送者身份两列（外部平台 ID 与发送时显示名）；
--   本地 / MCP 消息两列为 NULL，既有行为不变。
-- - 新增 session_members 表：记录会话内各外部发言者的显示名、群名片、
--   角色与首末见时间，供成员映射与展示。
--
-- 约定:
-- - 只增不删：不修改既有列、不重建表；新增列与新增表均向后兼容。
-- - 时间为 Unix 毫秒（与全体时间列一致）。
--
-- 字段约定:
-- - messages.sender_ref: 外部平台 ID（如 QQ uid）；本地消息为 NULL。
-- - messages.sender_name: 发送时显示名；本地消息为 NULL。
-- - session_members.platform_ref: 外部平台 ID；(session_id, platform_ref) 唯一。
-- - session_members.role: owner / admin / member，可空。

ALTER TABLE messages ADD COLUMN sender_ref TEXT;
ALTER TABLE messages ADD COLUMN sender_name TEXT;

CREATE INDEX idx_messages_sender_ref ON messages(sender_ref);

CREATE TABLE session_members (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    platform_ref   TEXT NOT NULL,
    name           TEXT NOT NULL DEFAULT '',
    group_nickname TEXT,
    role           TEXT,
    first_seen_at  INTEGER NOT NULL DEFAULT 0,
    last_seen_at   INTEGER NOT NULL DEFAULT 0,
    UNIQUE(session_id, platform_ref)
);
