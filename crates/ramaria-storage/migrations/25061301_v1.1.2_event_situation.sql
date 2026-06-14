-- =========================================================
-- Phase 1.1.2: 新增 memory_events.situation_strength 列
-- 
-- 用途: 将源 L1 的情境强度传播到 L2 事件，供 Phase A 统计
--       加权计算使用。存量数据默认 NULL（等效 3，中性情境）。
-- =========================================================

ALTER TABLE memory_events ADD COLUMN situation_strength INTEGER;
