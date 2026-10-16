-- =========================================================
-- 消息撤回标记与表情资产表
-- =========================================================
-- 目的:
-- - messages 追加 recalled_at 列（可空）：该消息在平台上被撤回的时刻，
--   NULL = 未撤回；撤回行保留原文与时间，撤回状态由本列表达。
-- - 新增 sticker_assets 表：登记聊天平台表情图片的本地资产（文件、md5、描述、标签）。
-- - 新增 md5 索引，配合按内容复用描述的查询。
--
-- 约定:
-- - 只增不删：不修改既有列与既有表；新增列、新增表、新增索引均向后兼容。
-- - 时间为 Unix 毫秒（与全体时间列一致）。
-- - 存量行 recalled_at 保持 NULL（历史消息无撤回信息）。
--
-- 字段约定:
-- - messages.recalled_at: 撤回时刻（Unix 毫秒）；NULL = 未撤回。
-- - sticker_assets.file_path: 资产目录内的文件名（不含目录前缀）。
-- - sticker_assets.tags: 标签 JSON 文本数组，默认 '[]'。
-- - sticker_assets.created_at / updated_at: Unix 毫秒。

ALTER TABLE messages ADD COLUMN recalled_at INTEGER;

CREATE TABLE sticker_assets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  file_name TEXT NOT NULL,
  file_path TEXT NOT NULL,
  md5 TEXT,
  size INTEGER,
  description TEXT,
  tags TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_sticker_assets_md5 ON sticker_assets(md5);
