-- =========================================================
-- 会话未读标记列（sessions.last_read_at）
-- =========================================================
-- 目的:
-- - sessions 表追加 last_read_at，记录会话最近一次被标记为已读的时间；
--   未读判定 = 存在本地助手消息晚于该时间（用户发言与导入历史不计）。
-- - 存量会话一律回填为其最大消息时间：旧库升级后既有会话视为已读，零虚假未读。
--
-- 约定:
-- - 只增不删：不修改既有列、不重建表；新增列均向后兼容。
-- - 时间为 Unix 毫秒（与全体时间列一致）；不做物化写入，未读数按需聚合查询。
--
-- 字段约定:
-- - last_read_at = 该会话被标记为已读的时间戳；0 表示从未标记。

ALTER TABLE sessions ADD COLUMN last_read_at INTEGER NOT NULL DEFAULT 0;

UPDATE sessions SET last_read_at = COALESCE(
    (SELECT MAX(m.created_at) FROM messages m WHERE m.session_id = sessions.id), 0
);
