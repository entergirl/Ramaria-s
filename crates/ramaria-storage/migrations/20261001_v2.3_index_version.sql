-- =========================================================
-- 索引版本：新库首启按"未构建"口径修正
-- =========================================================
-- 目的:
-- - 基线 schema 预置 `index_version = '1'`（已构建口径），使空库首启跳过真实构建、
--   状态机不进入"索引构建中"；但新建库实际尚未加载任何记忆，应走
--   "构建 → 就绪"的真实链路。
-- - 本迁移把该预置值修正为 '0'（未构建）：新库首次启动由入口完成一次构建并写回 '1'。
--
-- 边界:
-- - 仅当库内完全没有业务数据（sessions / messages / memory_l1 / memory_events
--   四表均无行）时生效：新库首启（四表皆空）被修正；既有库一律保持原值不动
--   （已写回 '1' 的库不受影响），本迁移对存量数据零改动。
-- - 空库被修正后，下次启动会执行一次空索引构建（幂等无害），随后写回 '1'。
-- - 仅对值为 '1' 的键生效，不覆盖其它来源写入的值。
UPDATE schema_meta
SET value = '0'
WHERE key = 'index_version'
  AND value = '1'
  AND NOT EXISTS (SELECT 1 FROM sessions)
  AND NOT EXISTS (SELECT 1 FROM messages)
  AND NOT EXISTS (SELECT 1 FROM memory_l1)
  AND NOT EXISTS (SELECT 1 FROM memory_events);
