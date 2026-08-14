-- =========================================================
-- Ramaria v1.5 M4 Migration —— L1 上下文感知生成支撑列
--
-- 内容:
-- 1. memory_l1.continuation 列（v1.5 B2，§6.3）：
--    与上一对话块的话题延续关系枚举（延续/转折/无关），
--    NULL = 无上一块（首块/独立摘要路径，等同 v1.4 行为）。
--
-- 幂等性说明:
-- - 使用 ALTER TABLE ... ADD COLUMN；重复执行会报 duplicate column 错误。
--   sqlx migrate 机制保证每个 migration 只执行一次；
--   如需手动重跑，请先确认列已存在（与 v1.4 utt migration 的 CREATE IF NOT EXISTS
--   策略不同，本迁移为纯增量加列，无存量数据转换）。
-- - 纯新增可空列：既有行 continuation 全部为 NULL，读取端按 None 处理，
--   不触发任何存量数据迁移。
-- =========================================================

ALTER TABLE memory_l1 ADD COLUMN continuation TEXT;
