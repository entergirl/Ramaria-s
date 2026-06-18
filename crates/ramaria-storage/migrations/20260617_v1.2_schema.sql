-- =========================================================
-- v1.2: Session-Persona 绑定 + 底层动机标注预埋
--
-- 用途:
-- 1. sessions.persona_uid: 后端主导 persona 状态，session 创建时写入当前对话人格
-- 2. memory_events.motives: 底层动机标注 Schema 预埋（v1.3 激活）
--
-- 说明:
-- - 均为增量 ADD COLUMN DEFAULT NULL，兼容存量数据。
-- - 旧 session 的 persona_uid 为 NULL 时，send_message 回退前端传参。
-- - memory_events.motives 在 v1.2 期间不修改任何业务逻辑（L2 提取、Phase A/B 均不涉及）。
--   v1.3 激活时仅需改 prompt 和统计逻辑，不需再做 schema 变更。
-- =========================================================

-- Session-Persona 绑定: 记录创建 session 时使用的对话人格
ALTER TABLE sessions ADD COLUMN persona_uid TEXT;

-- 底层动机标注预埋: TEXT/NULLABLE，v1.3 激活
ALTER TABLE memory_events ADD COLUMN motives TEXT;
