-- =========================================================
-- Ramaria 数据库完整 Schema（合并基线）
--
-- 说明:
-- - 由历史增量迁移合并而成，作为单一基线迁移文件。
-- - 时间字段统一 INTEGER（Unix 毫秒）。
-- - DDL 按依赖顺序排列：先建全部表，再执行列级增量变更。
-- =========================================================

-- =========================================================
-- 公共层（无外键依赖）
-- =========================================================

CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 默认 schema 版本 1，索引版本 1
INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('schema_version', '1');
INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('index_version', '1');

CREATE TABLE personas (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    uid        TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,       -- user / rama / char / anim / oc / hist
    seq        INTEGER NOT NULL,
    source     TEXT NOT NULL,       -- local / qq / wechat / telegram / manual / network
    ref_id     TEXT,                -- 来源方原始 ID
    avatar     TEXT,
    config     TEXT,                -- JSON 个性配置
    active     INTEGER NOT NULL DEFAULT 1,  -- 1=启用, 0=停用
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_personas_kind_source_ref
    ON personas(kind, source, ref_id) WHERE ref_id IS NOT NULL;

CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,        -- UUID v4
    started_at INTEGER NOT NULL,
    ended_at   INTEGER
);

-- =========================================================
-- L1 层（FK→sessions, personas）
-- =========================================================

CREATE TABLE memory_l1 (
    id              TEXT PRIMARY KEY,     -- UUID v4
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    summary         TEXT NOT NULL,
    keywords        TEXT,
    time_period     TEXT,
    atmosphere      TEXT,
    valence         REAL NOT NULL DEFAULT 0.0,
    salience        REAL NOT NULL DEFAULT 0.5,
    absorbed        INTEGER NOT NULL DEFAULT 0,   -- 0=未吸收, 1=已吸收
    created_at      INTEGER NOT NULL,
    last_accessed_at INTEGER,
    persona_uid     TEXT REFERENCES personas(uid),
    context_json    TEXT
);
CREATE INDEX idx_memory_l1_session ON memory_l1(session_id);
CREATE INDEX idx_memory_l1_absorbed ON memory_l1(absorbed);
CREATE INDEX idx_memory_l1_persona ON memory_l1(persona_uid);

-- =========================================================
-- L2 层 — memory_events（FK→personas）先于引用它的表创建
-- =========================================================

CREATE TABLE memory_events (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid      TEXT NOT NULL REFERENCES personas(uid),
    title            TEXT NOT NULL,
    summary          TEXT NOT NULL,
    keywords         TEXT,
    participants     TEXT,               -- JSON 数组
    start            INTEGER NOT NULL,
    "end"            INTEGER NOT NULL,
    confidence       REAL NOT NULL DEFAULT 0.5,
    salience         REAL NOT NULL DEFAULT 0.5,
    valence          REAL NOT NULL DEFAULT 0.0,
    presentation     TEXT NOT NULL DEFAULT 'mixed',  -- objective / subjective / mixed
    share            REAL NOT NULL DEFAULT 0.5,
    attitude         TEXT,
    paraphrase       TEXT,
    absorbed         INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    last_accessed_at INTEGER,
    indexed_at       INTEGER,
    index_version    INTEGER
);
CREATE INDEX idx_memory_events_persona_start ON memory_events(persona_uid, start);
CREATE INDEX idx_memory_events_share ON memory_events(share);

-- =========================================================
-- L2 层 — 事件关系 / 溯源（FK→memory_events, memory_l1）
-- =========================================================

CREATE TABLE event_relations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id    INTEGER NOT NULL REFERENCES memory_events(id),
    to_id      INTEGER NOT NULL REFERENCES memory_events(id),
    kind       TEXT NOT NULL,    -- CausedBy / PartOf / RelatedTo / ContinuedBy / Contradicts / Timeline
    weight     REAL NOT NULL DEFAULT 0.5,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_event_relations_from ON event_relations(from_id);
CREATE INDEX idx_event_relations_to ON event_relations(to_id);

CREATE TABLE event_sources (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id INTEGER NOT NULL REFERENCES memory_events(id),
    l1_id    TEXT NOT NULL REFERENCES memory_l1(id),
    weight   REAL NOT NULL DEFAULT 1.0,
    UNIQUE(event_id, l1_id)
);

-- =========================================================
-- L2 层 — 事实库（FK→personas, memory_events, memory_l1）
-- =========================================================

CREATE TABLE persona_facts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid  TEXT NOT NULL REFERENCES personas(uid),
    field        TEXT NOT NULL,        -- ProfileField 值
    content      TEXT NOT NULL,
    source       TEXT NOT NULL,        -- l1 / event / manual
    ref_event_id INTEGER REFERENCES memory_events(id),
    ref_l1_id    TEXT REFERENCES memory_l1(id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX idx_persona_facts_uid_field ON persona_facts(persona_uid, field);

-- =========================================================
-- L3 层 — 性格画像（FK→personas, memory_events, memory_l1）
-- =========================================================

CREATE TABLE personality_traits (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid  TEXT NOT NULL REFERENCES personas(uid),
    layer        TEXT NOT NULL,          -- base / primary / accent
    trait        TEXT NOT NULL,          -- 标签词
    meaning      TEXT NOT NULL,
    not_meaning  TEXT,
    trigger      TEXT,
    suppress     TEXT,
    related      TEXT,
    seq          INTEGER NOT NULL DEFAULT 0,
    source       TEXT NOT NULL,          -- l1 / event / manual / inferred
    ref_event_id INTEGER REFERENCES memory_events(id),
    ref_l1_id    TEXT REFERENCES memory_l1(id),
    confidence   REAL NOT NULL DEFAULT 0.0,
    evidence     REAL NOT NULL DEFAULT 0.0,
    consistency  REAL NOT NULL DEFAULT 0.0,
    status       TEXT NOT NULL DEFAULT 'active',  -- active / deprecated / historical
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX idx_personality_traits_uid_layer ON personality_traits(persona_uid, layer, seq);
CREATE INDEX idx_personality_traits_uid_status ON personality_traits(persona_uid, status);

-- =========================================================
-- L2/L3 层 — 证据链 / 聚类快照 / Few-shot
-- =========================================================

CREATE TABLE trait_evidence (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    trait_id   INTEGER NOT NULL REFERENCES personality_traits(id),
    event_id   INTEGER NOT NULL REFERENCES memory_events(id),
    direction  TEXT NOT NULL,   -- support / contradict / neutral
    score      REAL NOT NULL,   -- -1.0..1.0
    decay      REAL NOT NULL DEFAULT 1.0,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_trait_evidence_trait ON trait_evidence(trait_id);
CREATE INDEX idx_trait_evidence_event ON trait_evidence(event_id);

CREATE TABLE persona_cluster_snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid   TEXT NOT NULL REFERENCES personas(uid),
    category      TEXT NOT NULL,
    cluster_label TEXT NOT NULL,
    samples       TEXT,                  -- JSON 数组
    count         INTEGER NOT NULL DEFAULT 0,
    is_current    INTEGER NOT NULL DEFAULT 1,  -- 1=最新, 0=历史
    created_at    INTEGER NOT NULL
);
CREATE INDEX idx_cluster_snapshots_uid_cat ON persona_cluster_snapshots(persona_uid, category, is_current);

CREATE TABLE persona_examples (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid TEXT NOT NULL REFERENCES personas(uid),
    partner     TEXT NOT NULL,
    reply       TEXT NOT NULL,
    session_id  TEXT REFERENCES sessions(id),
    context     TEXT,
    valence     REAL NOT NULL DEFAULT 0.0,
    tags        TEXT,
    selected    INTEGER NOT NULL DEFAULT 0,  -- 1=当前生效, 0=候选库
    length      INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
CREATE INDEX idx_persona_examples_uid_sel ON persona_examples(persona_uid, selected);

-- =========================================================
-- L0 层 — messages（FK→sessions, personas）
-- =========================================================

CREATE TABLE messages (
    id               TEXT PRIMARY KEY,  -- UUID v4
    session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role             TEXT NOT NULL,
    content          TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    source           TEXT NOT NULL,
    import_fingerprint TEXT,
    persona_uid      TEXT REFERENCES personas(uid)
);
CREATE INDEX idx_messages_session ON messages(session_id);
CREATE INDEX idx_messages_created_at ON messages(created_at);
CREATE INDEX idx_messages_persona ON messages(persona_uid);

-- =========================================================
-- 基础设施层 — 关键词 / BM25 / 图谱
-- =========================================================

CREATE TABLE keyword_pool (
    keyword       TEXT PRIMARY KEY,
    use_count     INTEGER NOT NULL DEFAULT 0,
    last_used_at  INTEGER,
    created_at    INTEGER NOT NULL,
    canonical_id  INTEGER,
    alias_status  TEXT    -- confirmed / pending / canonical
);
CREATE INDEX idx_keyword_alias ON keyword_pool(alias_status);

CREATE TABLE bm25_index (
    doc_id     INTEGER NOT NULL,
    layer      TEXT NOT NULL,       -- l0 / l1 / event
    tokens_json TEXT NOT NULL,
    PRIMARY KEY (doc_id, layer)
);

CREATE TABLE graph_nodes (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_name  TEXT NOT NULL UNIQUE,
    entity_type  TEXT NOT NULL,
    source_l1_id TEXT REFERENCES memory_l1(id),
    created_at   INTEGER NOT NULL,
    use_count    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE graph_edges (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    source_node_id INTEGER NOT NULL REFERENCES graph_nodes(id),
    target_node_id INTEGER NOT NULL REFERENCES graph_nodes(id),
    relation_type  TEXT NOT NULL,
    relation_detail TEXT,
    source_l1_id   TEXT REFERENCES memory_l1(id),
    created_at     INTEGER NOT NULL
);
CREATE INDEX idx_graph_edges_source ON graph_edges(source_node_id);
CREATE INDEX idx_graph_edges_target ON graph_edges(target_node_id);
CREATE INDEX idx_graph_edges_l1 ON graph_edges(source_l1_id);

-- =========================================================
-- 基础设施层 — 隐私 / 后端配置 / 后台任务
-- =========================================================

CREATE TABLE privacy_consent (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    provider   TEXT NOT NULL,
    base_url   TEXT NOT NULL,
    timestamp  INTEGER NOT NULL,
    persistent INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_privacy_consent_provider_url ON privacy_consent(provider, base_url);

CREATE TABLE backend_config (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL
);

CREATE TABLE background_jobs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_type    TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    payload     TEXT,
    created_at  INTEGER NOT NULL,
    started_at  INTEGER,
    finished_at INTEGER,
    error       TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    max_retries INTEGER NOT NULL DEFAULT 3
);

-- =========================================================
-- 基础设施层 — 冲突队列 / 推送 / 设置
-- =========================================================

CREATE TABLE conflict_queue (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    source_l1_id  TEXT REFERENCES memory_l1(id),
    field         TEXT NOT NULL,
    old_content   TEXT,
    new_content   TEXT,
    conflict_desc TEXT,
    conflict_type TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',
    created_at    INTEGER NOT NULL,
    resolved_at   INTEGER
);
CREATE INDEX idx_conflict_status ON conflict_queue(status);

CREATE TABLE pending_push (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    content    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending',
    sent_at    INTEGER
);
CREATE INDEX idx_pending_push_status ON pending_push(status);

CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- =========================================================
-- 列级增量（历史迁移合并）
-- =========================================================

-- 情境强度加权（memory_events / memory_l1，1-5 级，NULL 等效 3 中性）
ALTER TABLE memory_events ADD COLUMN situation_strength INTEGER;
ALTER TABLE memory_l1 ADD COLUMN situation_strength INTEGER;

-- 人格简介（personas，面向用户的多角色管理描述）
ALTER TABLE personas ADD COLUMN description TEXT;

-- Session 绑定当前对话人格（persona_uid）
ALTER TABLE sessions ADD COLUMN persona_uid TEXT;

-- 底层动机标注（memory_events，JSON 数组字符串）
ALTER TABLE memory_events ADD COLUMN motives TEXT;

-- 关键词倒排索引表（关键词 → 业务文档引用）
CREATE TABLE IF NOT EXISTS keyword_refs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    keyword_id  TEXT    NOT NULL REFERENCES keyword_pool(keyword) ON DELETE CASCADE,
    doc_type    TEXT    NOT NULL,   -- 'l1' / 'l2'
    doc_id      TEXT    NOT NULL,   -- L1: UUID字符串, L2: i64字符串
    persona_uid TEXT    NOT NULL,
    weight      REAL    NOT NULL DEFAULT 1.0,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_keyword_refs_keyword
    ON keyword_refs(keyword_id);

CREATE INDEX IF NOT EXISTS idx_keyword_refs_doc
    ON keyword_refs(doc_type, doc_id);

-- 聚类快照跨版本语义标签
ALTER TABLE persona_cluster_snapshots ADD COLUMN semantic_label TEXT;
ALTER TABLE persona_cluster_snapshots ADD COLUMN semantic_label_embedding BLOB;

-- L1 证据片段（JSON 数组字符串）
ALTER TABLE memory_l1 ADD COLUMN evidence_notes TEXT;
