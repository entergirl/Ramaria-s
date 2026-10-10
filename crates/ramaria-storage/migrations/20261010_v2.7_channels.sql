-- =========================================================
-- 社交通道绑定（channel_bindings 表 + messages 平台消息 ID）
-- =========================================================
-- 目的:
-- - 新增 channel_bindings 表：记录外部会话（私聊 / 群聊）与本机侧人格的绑定、
--   启用开关与主动投递开关，供社交通道按外部会话定位生效人格。
-- - messages 追加平台消息 ID 列（可空 + 部分唯一索引）：同一会话内平台消息
--   唯一，重复事件由数据库兜底拒绝；本地 / MCP / 导入旧路径恒为 NULL，行为不变。
--
-- 约定:
-- - 只增不删：不修改既有列与既有表；新增列、新增表与新增索引均向后兼容。
-- - 时间为 Unix 毫秒（与全体时间列一致）。
--
-- 字段约定:
-- - channel_bindings.channel: 通道标识（qq / telegram）。
-- - channel_bindings.external_ref: 会话外部标识（c2c:{uin} 私聊 / group:{群号} 群聊）；
--   (channel, external_ref) 唯一。
-- - channel_bindings.persona_uid: 本机侧人格；NULL = 未覆盖，生效时回退通道级默认。
-- - channel_bindings.enabled: 是否启用该绑定的入站处理（默认 1）。
-- - channel_bindings.allow_proactive: 是否允许对该会话主动投递（默认 0）。
-- - messages.platform_message_id: 平台消息 ID；本地 / 导入旧路径为 NULL；
--   按 (session_id, platform_message_id) 部分唯一（NULL 不受约束）。

CREATE TABLE channel_bindings (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    channel         TEXT NOT NULL,
    external_ref    TEXT NOT NULL,
    persona_uid     TEXT,
    enabled         INTEGER NOT NULL DEFAULT 1,
    allow_proactive INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0,
    UNIQUE(channel, external_ref)
);

ALTER TABLE messages ADD COLUMN platform_message_id TEXT;

CREATE UNIQUE INDEX idx_messages_platform_message_id
    ON messages(session_id, platform_message_id) WHERE platform_message_id IS NOT NULL;
