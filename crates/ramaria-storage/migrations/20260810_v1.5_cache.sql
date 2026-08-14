-- =========================================================
-- Ramaria v1.5 Migration —— 三层生成缓存（C，D-V15-008）
--
-- 内容:
-- 1. llm_response_cache 表：LLM 响应精确缓存。
--    key = sha256(model_id + prompt 模板版本 + prompt)，命中直接复用，
--    覆盖重跑/重试/失败恢复导入与生成管线场景（不重复花费 API 账单）。
--    隐私红线：只存响应，不存原文输入（表结构无 prompt/原文列）。
-- 2. l2_cluster_fingerprints 表：L2 聚类去重指纹。
--    记录"已聚类且无产出"的 L1 集合指纹（SHA-256 集合指纹），
--    同集合未吸收 L1 不重复聚类；集合变更后指纹变化自动重聚类。
--
-- 幂等性说明:
-- - 全部使用 CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS，
--   整个 SQL 文件可安全重复执行。
-- - 本 migration 不修改任何既有表（增量迁移，见 v1.5 破坏性变更声明）。
-- =========================================================

-- ---------------------------------------------------------
-- 1. llm_response_cache 表
-- ---------------------------------------------------------

CREATE TABLE IF NOT EXISTS llm_response_cache (
    -- 缓存键：sha256(model_id + template_version + prompt) 的 hex 摘要。
    -- 只存哈希不存 prompt 原文（隐私最小暴露）。
    key TEXT PRIMARY KEY,
    -- LLM 响应文本（唯一存储的内容）。
    response TEXT NOT NULL,
    -- 模型标识（BackendConfig.capability.model_id），审计用途。
    model_id TEXT NOT NULL,
    -- Prompt 模板版本（ChatRequest.template_version），审计用途。
    template_version TEXT NOT NULL,
    -- 首次写入时间（epoch ms）。
    created_at INTEGER NOT NULL,
    -- 最近一次命中时间（epoch ms），用于 LRU 淘汰。
    last_accessed_at INTEGER NOT NULL,
    -- 累计命中次数，用于审计与淘汰统计。
    hit_count INTEGER NOT NULL DEFAULT 0
);

-- 按访问时间淘汰（LRU）与按写入时间淘汰（FIFO）共用索引。
CREATE INDEX IF NOT EXISTS idx_llm_response_cache_accessed
    ON llm_response_cache(last_accessed_at);
CREATE INDEX IF NOT EXISTS idx_llm_response_cache_created
    ON llm_response_cache(created_at);

-- ---------------------------------------------------------
-- 2. l2_cluster_fingerprints 表
-- ---------------------------------------------------------

CREATE TABLE IF NOT EXISTS l2_cluster_fingerprints (
    -- 分析对象的人格标识。
    persona_uid TEXT NOT NULL,
    -- L1 集合指纹（SHA-256 hex，按 L1 id 排序后拼接计算）。
    fingerprint TEXT NOT NULL,
    -- 记录时间（epoch ms）。
    created_at INTEGER NOT NULL,
    -- 同一 persona 下指纹唯一。
    PRIMARY KEY (persona_uid, fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_l2_cluster_fingerprints_created
    ON l2_cluster_fingerprints(created_at);
