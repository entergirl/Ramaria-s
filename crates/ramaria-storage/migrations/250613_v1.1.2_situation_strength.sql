-- =========================================================
-- Phase 1.1.2: 新增 situation_strength 列
-- 
-- 用途: 支持情境强度加权（1-5级）用于 Phase A 统计。
-- 存量数据默认为 NULL（等效 3，中性情境），避免 NULL 值
-- 被加权逻辑跳过。
--
-- 说明:
-- - 此列为 Phase 1.1.2 准备，Phase 1.1.0 的代码已支持读写此列。
-- - NULL 在 MemoryL1 中映射为 `situation_strength: None`，
--   summarizer 注入阶段回退到 `Some(3)`。
-- =========================================================

ALTER TABLE memory_l1 ADD COLUMN situation_strength INTEGER;
