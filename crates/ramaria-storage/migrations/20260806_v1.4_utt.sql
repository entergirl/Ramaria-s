-- =========================================================
-- Ramaria v1.4 Migration —— utt_blocks 表 + evidence_notes 结构化
--
-- 内容:
-- 1. evidence_notes 存量迁移（一次性破坏性变更，见 docs/dev-1.4/v1.4-decisions.md）
--    旧格式 JSON 字符串数组 → 新格式对象数组 [{text, time?, who?, cause?}]
--    字符串落 text 槽位，time/who/cause 置空；迁移前备份原值。
-- 2. utt_blocks 表（v1.4 新增，见 docs/dev-1.4/v1.4-decisions.md）：原文话语块存储，供检索/桥接/风格统计复用。
--
-- 幂等性说明:
-- - evidence_notes 迁移仅处理"首元素为字符串"的旧格式行；
--   已迁移的对象数组行不受影响，重复执行安全。
-- - utt_blocks 表使用 CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS，
--   整个 SQL 文件可安全重复执行（sqlx 机制之外的手动重跑同样安全）。
-- =========================================================

-- ---------------------------------------------------------
-- 1. evidence_notes 存量迁移
-- ---------------------------------------------------------

-- 1.1 迁移前备份原值（仅备份非空行）
CREATE TABLE IF NOT EXISTS memory_l1_evidence_notes_backup AS
SELECT id, evidence_notes
FROM memory_l1
WHERE evidence_notes IS NOT NULL AND evidence_notes <> '';

-- 1.2 旧字符串数组 → 新对象数组（字符串落 text 槽位，其余置空）
--     仅处理 json_valid 且首元素为字符串（'text' 类型）的旧格式行；
--     新格式对象数组（首元素 'object'）与空数组（无首元素）保持不变。
--     json_object 负责字符串转义；空数组不会被此 UPDATE 命中（保持原样）。
--     内层子查询按数组索引（key 列，字符串索引）排序，保证 group_concat 顺序稳定。
--     边缘情况（v1.4 M4 增强）：混合数组（如 ["旧字符串", {"text":"新对象"}]）
--     按元素类型逐项转换——字符串落 text 槽位、已是对象的元素原样保留，
--     数字/布尔/null 元素丢弃（group_concat 跳过 NULL），
--     避免旧数据中对象元素被错误嵌套进 text 字段导致读取失败。
--     注意：json_type(value) 对非 JSON 字符串直接报 malformed JSON 而非返回 NULL，
--     因此必须先以 json_valid(value) 短路守卫（AND 短路保证 json_type 不被误调）。
UPDATE memory_l1
SET evidence_notes = (
    SELECT '[' || group_concat(
        CASE
            WHEN json_valid(value) AND json_type(value) = 'object' THEN json(value)
            WHEN typeof(value) = 'text' THEN
                json_object('text', value, 'time', NULL, 'who', NULL, 'cause', NULL)
        END, ','
    ) || ']'
    FROM (
        SELECT value, key
        FROM json_each(memory_l1.evidence_notes)
        ORDER BY CAST(key AS INTEGER)
    )
)
WHERE evidence_notes IS NOT NULL
  AND json_valid(evidence_notes)
  AND json_type(evidence_notes, '$[0]') = 'text';

-- ---------------------------------------------------------
-- 2. utt_blocks 表（原文话语块）
-- ---------------------------------------------------------
-- 字段约定:
-- - persona_uid: 话语块归属人格（原文按 persona 严格隔离）。
-- - session_id: 来源会话。
-- - start_msg_id / end_msg_id: 块覆盖的消息区间（FK→messages.id）。
-- - block_text: 块内原文全文（不含元数据，按原文格式拼接）。
-- - msg_count: 块内消息条数。
-- - time_span_ms: 块内首末消息时间跨度（毫秒）。
-- - embedding: 块文本的向量（f32 小端 BLOB），None 表示未生成/不可用。

CREATE TABLE IF NOT EXISTS utt_blocks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid   TEXT NOT NULL REFERENCES personas(uid),
    session_id    TEXT NOT NULL REFERENCES sessions(id),
    start_msg_id  TEXT NOT NULL REFERENCES messages(id),
    end_msg_id    TEXT NOT NULL REFERENCES messages(id),
    block_text    TEXT NOT NULL,
    msg_count     INTEGER NOT NULL,
    time_span_ms  INTEGER NOT NULL,
    embedding     BLOB,
    created_at    INTEGER NOT NULL
);

-- 检索按 persona 过滤；会话级删除（会话清理）与桥接查询走 session 索引
CREATE INDEX IF NOT EXISTS idx_utt_blocks_persona ON utt_blocks(persona_uid);
CREATE INDEX IF NOT EXISTS idx_utt_blocks_session ON utt_blocks(session_id);
CREATE INDEX IF NOT EXISTS idx_utt_blocks_created_at ON utt_blocks(created_at);
