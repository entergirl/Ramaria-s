-- =========================================================
-- 消息附件（message_attachments 表）
-- =========================================================
-- 目的:
-- - 新增 message_attachments 表：记录消息所携带附件的引用、指纹、尺寸与
--   理解结果，供导入采集落库、图片理解回填与读取渲染消费。
--
-- 约定:
-- - 只增不删：不修改既有列与既有表；新增表向后兼容。
-- - 时间为 Unix 毫秒（与全体时间列一致）。
--
-- 字段约定:
-- - message_id: 归属消息 ID；消息删除时附件行级联删除。
-- - kind: 附件类型，取值 image / audio / video / file。
-- - source_ref: 导出根相对路径或 URL 引用（引用而非内容）。
-- - md5: 图片内容 md5（小写 hex）；平台未提供为 NULL。
-- - size / width / height: 可选的体积与尺寸信息。
-- - sub_type: 平台细分类型（如 photo / sticker）。
-- - status: 处理状态，取值 pending / done / failed / skipped。
-- - description: 理解结果文本；仅 status = done 有值。
-- - description_model: 产生描述的模型标识。

CREATE TABLE message_attachments (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id        TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL,
    source_ref        TEXT NOT NULL DEFAULT '',
    md5               TEXT,
    size              INTEGER,
    width             INTEGER,
    height            INTEGER,
    sub_type          TEXT,
    status            TEXT NOT NULL DEFAULT 'pending',
    description       TEXT,
    description_model TEXT,
    created_at        INTEGER NOT NULL DEFAULT 0,
    updated_at        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_attachments_message_id ON message_attachments(message_id);
CREATE INDEX idx_attachments_status ON message_attachments(status);
CREATE INDEX idx_attachments_md5 ON message_attachments(md5);
