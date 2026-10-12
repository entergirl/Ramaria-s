-- =========================================================
-- 记忆时间语义修复（memory_l1 发生时间列 + 存量回填）
-- =========================================================
-- 目的:
-- - memory_l1 追加 occurred_at 列（可空）：该摘要所涵盖对话块的开始时刻
--   （块内最早消息时间），作为衰减 / 排序 / 时效的第一时间基准。
-- - 存量回填：L1 按来源会话最早消息时间推导；L2 事件的 start/end 回归
--   源 L1 发生时间的 min/max（须在 L1 回填后执行）。
-- - 新增 persona 维度发生时间索引，配合按发生时间排序的查询。
--
-- 约定:
-- - 只增不删：不修改既有列与既有表；新增列、新增索引均向后兼容。
-- - 时间为 Unix 毫秒（与全体时间列一致）。
-- - 幂等：回填条件均带 NULL / EXISTS 守卫，可重复执行。
-- - LLM 标签 time_period 与审计时间 created_at 不受本迁移影响。
--
-- 字段约定:
-- - memory_l1.occurred_at: 块内最早消息时间；无消息的会话保持 NULL，
--   读取侧回退 created_at。

ALTER TABLE memory_l1 ADD COLUMN occurred_at INTEGER;

CREATE INDEX idx_memory_l1_persona_occurred ON memory_l1(persona_uid, occurred_at);

-- L1 回填：按来源会话最早消息时间推导（块级精确回填需逐块；首版按会话近似，
-- 多段 L1 共享同一值，可接受）
UPDATE memory_l1 SET occurred_at = (
    SELECT MIN(m.created_at) FROM messages m WHERE m.session_id = memory_l1.session_id
) WHERE occurred_at IS NULL AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = memory_l1.session_id);

-- L2 回填：事件 start/end 回归源 L1 发生时间的 min/max（须在 L1 回填后执行；
-- 无来源或源无发生时间时保持原值）
UPDATE memory_events SET
  start = COALESCE((SELECT MIN(l.occurred_at) FROM event_sources s JOIN memory_l1 l ON l.id = s.l1_id WHERE s.event_id = memory_events.id), start),
  "end" = COALESCE((SELECT MAX(l.occurred_at) FROM event_sources s JOIN memory_l1 l ON l.id = s.l1_id WHERE s.event_id = memory_events.id), "end")
WHERE EXISTS (SELECT 1 FROM event_sources s WHERE s.event_id = memory_events.id);
