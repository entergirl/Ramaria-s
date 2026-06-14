-- =========================================================
-- Phase 6 (v1.1): personas 表新增 description 列
-- 
-- 用途: 支持多角色管理 GUI 的人格描述字段。
-- 用户可在人格详情页编辑此字段，用于记录人格的简要介绍。
--
-- 说明:
-- - 此为可选字段（TEXT, nullable），存量数据为 NULL。
-- - 前端人格卡片网格中显示为预览文字。
-- - 与 config 列不同：description 是面向用户的简短文本，
--   config 是面向系统的 JSON/TOML 个性配置。
-- =========================================================

ALTER TABLE personas ADD COLUMN description TEXT;
