-- Ramaria v1.0 正式数据库 schema
-- Phase 1: Core + Storage 初始迁移
--
-- 设计约定:
-- - 所有 ID 使用 UUID v4，SQLite TEXT 存储
-- - 所有时间使用 Unix 毫秒时间戳，SQLite INTEGER 存储
-- - 布尔值使用 INTEGER (0/1)
-- - 外键关系在应用层维护，SQLite 不强制 FOREIGN KEY（避免迁移顺序问题）

-- =========================================================
-- Schema 元信息
-- =========================================================

CREATE TABLE IF NOT EXISTS schema_meta (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL
);

-- 初始化默认 schema 版本
INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('schema_version', '1');
INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('index_version', '0');

-- =========================================================
-- 会话
-- =========================================================

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER            -- NULL 表示未关闭
);

CREATE INDEX IF NOT EXISTS idx_sessions_ended_at ON sessions(ended_at);

-- =========================================================
-- L0 原始消息
-- =========================================================

CREATE TABLE IF NOT EXISTS messages (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    role        TEXT NOT NULL,       -- user / assistant / system / tool
    content     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    source      TEXT NOT NULL,       -- local / online
    fingerprint TEXT                 -- SHA-256 前 16 位 hex，历史导入去重
);

CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_messages_fingerprint ON messages(fingerprint);
CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(session_id, created_at);

-- =========================================================
-- L1 单次会话摘要
-- =========================================================

CREATE TABLE IF NOT EXISTS memory_l1 (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    summary         TEXT NOT NULL,
    keywords        TEXT,            -- 逗号分隔关键词
    time_period     TEXT,            -- 清晨/上午/下午/傍晚/夜间/深夜
    atmosphere      TEXT,            -- 气氛描述
    valence         REAL NOT NULL DEFAULT 0.0,   -- -1.0..1.0
    salience        REAL NOT NULL DEFAULT 0.5,   -- 0.0..1.0
    absorbed        INTEGER NOT NULL DEFAULT 0,  -- 是否已被 L2 吸收
    created_at      INTEGER NOT NULL,
    last_accessed_at INTEGER,
    indexed_at      INTEGER,        -- 最近被索引的时间
    index_version   INTEGER         -- 索引时的版本号
);

CREATE INDEX IF NOT EXISTS idx_memory_l1_session_id ON memory_l1(session_id);
CREATE INDEX IF NOT EXISTS idx_memory_l1_absorbed ON memory_l1(absorbed);
CREATE INDEX IF NOT EXISTS idx_memory_l1_created_at ON memory_l1(created_at);

-- =========================================================
-- L2 时间段聚合摘要
-- =========================================================

CREATE TABLE IF NOT EXISTS memory_l2 (
    id              TEXT PRIMARY KEY,
    summary         TEXT NOT NULL,
    keywords        TEXT,
    period_start    INTEGER NOT NULL,
    period_end      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    last_accessed_at INTEGER,
    indexed_at      INTEGER,
    index_version   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_memory_l2_created_at ON memory_l2(created_at);
CREATE INDEX IF NOT EXISTS idx_memory_l2_period ON memory_l2(period_start, period_end);

-- =========================================================
-- L2 → L1 溯源关系
-- =========================================================

CREATE TABLE IF NOT EXISTS l2_sources (
    l2_id   TEXT NOT NULL,
    l1_id   TEXT NOT NULL,
    PRIMARY KEY (l2_id, l1_id)
);

CREATE INDEX IF NOT EXISTS idx_l2_sources_l1 ON l2_sources(l1_id);

-- =========================================================
-- L3 用户画像
-- =========================================================

CREATE TABLE IF NOT EXISTS user_profile (
    id          TEXT PRIMARY KEY,
    field       TEXT NOT NULL,       -- basic_info / personal_status / interests / social / history / recent_context
    content     TEXT NOT NULL,
    source_l1_id TEXT,               -- 来源 L1 ID
    status      TEXT NOT NULL DEFAULT 'approved',  -- approved / pending / rejected
    is_current  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_user_profile_field ON user_profile(field);
CREATE INDEX IF NOT EXISTS idx_user_profile_current ON user_profile(is_current);

-- =========================================================
-- 隐私确认记录
-- =========================================================

CREATE TABLE IF NOT EXISTS privacy_consent (
    provider    TEXT NOT NULL,
    base_url    TEXT NOT NULL,
    timestamp   INTEGER NOT NULL,
    persistent  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (provider, base_url)
);

-- =========================================================
-- 非敏感后端配置
-- =========================================================

CREATE TABLE IF NOT EXISTS backend_config (
    id              INTEGER PRIMARY KEY CHECK (id = 1),  -- 单行设计
    config_json     TEXT NOT NULL                         -- 完整 BackendConfig JSON
);

-- =========================================================
-- 后台任务
-- =========================================================

CREATE TABLE IF NOT EXISTS background_jobs (
    id              TEXT PRIMARY KEY,
    job_type        TEXT NOT NULL,    -- l1_summary / l2_merge / index_rebuild / model_download
    status          TEXT NOT NULL,    -- pending / running / failed / done
    payload_json    TEXT,             -- 任务参数 JSON
    error_message   TEXT,
    created_at      INTEGER NOT NULL,
    started_at      INTEGER,
    finished_at     INTEGER,
    retry_count     INTEGER NOT NULL DEFAULT 0,
    max_retries     INTEGER NOT NULL DEFAULT 3
);

CREATE INDEX IF NOT EXISTS idx_background_jobs_status ON background_jobs(status);
CREATE INDEX IF NOT EXISTS idx_background_jobs_type_status ON background_jobs(job_type, status);

-- =========================================================
-- BM25 索引持久化
-- =========================================================

CREATE TABLE IF NOT EXISTS bm25_index (
    doc_id      TEXT NOT NULL,
    layer       TEXT NOT NULL,       -- l0 / l1 / l2
    tokens_json TEXT NOT NULL,       -- JSON 数组，存储分词结果
    PRIMARY KEY (doc_id, layer)
);

-- =========================================================
-- 知识图谱节点
-- =========================================================

CREATE TABLE IF NOT EXISTS graph_nodes (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    node_type   TEXT NOT NULL,       -- keyword / entity / topic
    layer       TEXT NOT NULL,       -- l1 / l2
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_graph_nodes_type ON graph_nodes(node_type);

-- =========================================================
-- 知识图谱边
-- =========================================================

CREATE TABLE IF NOT EXISTS graph_edges (
    id              TEXT PRIMARY KEY,
    source_node_id  TEXT NOT NULL,
    target_node_id  TEXT NOT NULL,
    weight          REAL NOT NULL DEFAULT 1.0,
    edge_type       TEXT NOT NULL DEFAULT 'cooccurrence',
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_graph_edges_source ON graph_edges(source_node_id);
CREATE INDEX IF NOT EXISTS idx_graph_edges_target ON graph_edges(target_node_id);
