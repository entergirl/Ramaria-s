-- Ramaria schema 快照（自动生成，请勿手改）
-- 生成命令: python scripts/check_schema.py --update
-- migration: 20261018_v3.0_schema.sql
-- 统计: 表 29 / 列 259 / 显式索引 45 / 外键 28 / schema_version=3

-- table: backend_config
CREATE TABLE backend_config (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL
)

-- table: background_jobs
CREATE TABLE background_jobs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_type    TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    payload     TEXT,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER,
    error       TEXT
)

-- table: behavior_rules
CREATE TABLE behavior_rules (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid TEXT NOT NULL,
    situation   TEXT NOT NULL,      -- BehaviorSituation JSON
    reaction    TEXT,               -- NULL = 候选规则（仅参数注入）
    params      TEXT NOT NULL,      -- BehaviorParams JSON
    avoid       TEXT NOT NULL DEFAULT '[]',
    evidence    TEXT NOT NULL DEFAULT '[]',
    confidence  REAL NOT NULL DEFAULT 0,
    stability   REAL NOT NULL DEFAULT 0,
    source      TEXT NOT NULL DEFAULT 'auto',
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
)

-- table: channel_bindings
CREATE TABLE channel_bindings (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    channel         TEXT NOT NULL,
    external_ref    TEXT NOT NULL,
    persona_uid     TEXT,
    enabled         INTEGER NOT NULL DEFAULT 1,
    allow_proactive INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0,
    UNIQUE(channel, external_ref)
)

-- table: event_relations
CREATE TABLE event_relations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id    INTEGER NOT NULL REFERENCES memory_events(id),
    to_id      INTEGER NOT NULL REFERENCES memory_events(id),
    kind       TEXT NOT NULL,    -- CausedBy / PartOf / RelatedTo / ContinuedBy / Contradicts / Timeline
    weight     REAL NOT NULL DEFAULT 0.5,
    created_at INTEGER NOT NULL
)

-- table: event_sources
CREATE TABLE event_sources (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id INTEGER NOT NULL REFERENCES memory_events(id),
    l1_id    TEXT NOT NULL REFERENCES memory_l1(id),
    weight   REAL NOT NULL DEFAULT 1.0,
    UNIQUE(event_id, l1_id)
)

-- table: feedback_log
CREATE TABLE feedback_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid TEXT NOT NULL,
    target_type TEXT NOT NULL,      -- behavior_rule | persona_fact | personality_trait
    target_id   TEXT NOT NULL,
    signal_type TEXT NOT NULL,      -- edit | disable | correction | continue
    weight      REAL NOT NULL DEFAULT 1.0,
    session_id  TEXT,
    detail      TEXT,
    created_at  INTEGER NOT NULL
)

-- table: keyword_pool
CREATE TABLE keyword_pool (
    keyword      TEXT PRIMARY KEY,
    use_count    INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    created_at   INTEGER NOT NULL,
    canonical_id INTEGER,
    alias_status TEXT    -- confirmed / pending / canonical
)

-- table: l2_cluster_fingerprints
CREATE TABLE l2_cluster_fingerprints (
    persona_uid TEXT NOT NULL,
    fingerprint TEXT NOT NULL,   -- SHA-256 hex
    PRIMARY KEY (persona_uid, fingerprint)
)

-- table: llm_response_cache
CREATE TABLE llm_response_cache (
    key              TEXT PRIMARY KEY,   -- sha256(model_id + template_version + prompt)
    response         TEXT NOT NULL,      -- 只存响应，不存 prompt 原文
    model_id         TEXT NOT NULL,
    template_version TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    last_accessed_at INTEGER NOT NULL,
    hit_count        INTEGER NOT NULL DEFAULT 0
)

-- table: memory_events
CREATE TABLE memory_events (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid        TEXT NOT NULL REFERENCES personas(uid),
    title              TEXT NOT NULL,
    summary            TEXT NOT NULL,
    keywords           TEXT,
    participants       TEXT,               -- JSON 数组
    start              INTEGER NOT NULL,
    "end"              INTEGER NOT NULL,
    confidence         REAL NOT NULL DEFAULT 0.5,
    salience           REAL NOT NULL DEFAULT 0.5,
    valence            REAL NOT NULL DEFAULT 0.0,
    presentation       TEXT NOT NULL DEFAULT 'mixed',  -- objective / subjective / mixed
    share              REAL NOT NULL DEFAULT 0.5,
    attitude           TEXT,
    paraphrase         TEXT,
    absorbed           INTEGER NOT NULL DEFAULT 0,
    created_at         INTEGER NOT NULL,
    last_accessed_at   INTEGER,
    situation_strength INTEGER,             -- 情境强度 1-5
    motives            TEXT                 -- 底层动机标注（JSON 数组字符串）
)

-- table: memory_l1
CREATE TABLE memory_l1 (
    id                 TEXT PRIMARY KEY,     -- UUID v4
    session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    summary            TEXT NOT NULL,
    keywords           TEXT,
    time_period        TEXT,
    atmosphere         TEXT,
    valence            REAL NOT NULL DEFAULT 0.0,
    salience           REAL NOT NULL DEFAULT 0.5,
    absorbed           INTEGER NOT NULL DEFAULT 0,   -- 0=未吸收, 1=已吸收
    created_at         INTEGER NOT NULL,
    last_accessed_at   INTEGER,
    persona_uid        TEXT REFERENCES personas(uid),
    context_json       TEXT,
    situation_strength INTEGER,             -- 情境强度 1-5（NULL 等效 3 中性）
    evidence_notes     TEXT,                -- 结构化对象数组 [{text,time?,who?,cause?}]
    continuation       TEXT,                -- 与上一对话块的延续/转折/无关枚举
    occurred_at        INTEGER              -- 块内最早消息时间；读取侧回退 created_at
)

-- table: memory_vectors
CREATE TABLE memory_vectors (
    doc_type   TEXT    NOT NULL,
    doc_id     TEXT    NOT NULL,
    model      TEXT    NOT NULL,
    dim        INTEGER NOT NULL,
    vector     BLOB    NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (doc_type, doc_id)
)

-- table: message_attachments
CREATE TABLE message_attachments (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id        TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL,            -- image / audio / video / file
    source_ref        TEXT NOT NULL DEFAULT '', -- 导出根相对路径或 URL 引用
    md5               TEXT,                     -- 图片内容 md5（小写 hex）
    size              INTEGER,
    width             INTEGER,
    height            INTEGER,
    sub_type          TEXT,                     -- 平台细分类型（photo / sticker）
    status            TEXT NOT NULL DEFAULT 'pending',  -- pending / done / failed / skipped
    description       TEXT,                     -- 理解结果文本；仅 done 有值
    description_model TEXT,                     -- 产生描述的模型标识
    created_at        INTEGER NOT NULL DEFAULT 0,
    updated_at        INTEGER NOT NULL DEFAULT 0
)

-- table: messages
CREATE TABLE messages (
    id                  TEXT PRIMARY KEY,  -- UUID v4
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role                TEXT NOT NULL,
    content             TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    source              TEXT NOT NULL,
    import_fingerprint  TEXT UNIQUE,  -- 跨文件导入去重：同指纹不可重复入库
    persona_uid         TEXT REFERENCES personas(uid),
    is_proactive        INTEGER NOT NULL DEFAULT 0,  -- 1 = 主动生成消息
    sender_ref          TEXT,                -- 外部平台 ID；本地消息为 NULL
    sender_name         TEXT,                -- 发送时显示名；本地消息为 NULL
    platform_message_id TEXT,                -- 平台消息 ID；(session_id, 平台 ID) 部分唯一
    recalled_at         INTEGER              -- 撤回时刻；NULL = 未撤回
)

-- table: persona_cluster_snapshots
CREATE TABLE persona_cluster_snapshots (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid              TEXT NOT NULL REFERENCES personas(uid),
    category                 TEXT NOT NULL,
    cluster_label            TEXT NOT NULL,
    samples                  TEXT,                  -- JSON 数组
    count                    INTEGER NOT NULL DEFAULT 0,
    is_current               INTEGER NOT NULL DEFAULT 1,  -- 1=最新, 0=历史
    created_at               INTEGER NOT NULL,
    semantic_label           TEXT,                  -- 跨版本簇语义标签
    semantic_label_embedding BLOB                   -- 标签 embedding（f32 小端）
)

-- table: persona_examples
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
)

-- table: persona_facts
CREATE TABLE persona_facts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid  TEXT NOT NULL REFERENCES personas(uid),
    field        TEXT NOT NULL,        -- ProfileField 值
    content      TEXT NOT NULL,
    source       TEXT NOT NULL,        -- event / manual / l1
    tier         TEXT NOT NULL DEFAULT 'stable',  -- stable / volatile / historical
    status       TEXT NOT NULL DEFAULT 'active',  -- active / superseded / candidate
    version_of   INTEGER REFERENCES persona_facts(id),  -- 覆盖时指向被替换事实 id
    confidence   REAL NOT NULL DEFAULT 0.0,
    keyword_hint TEXT,                -- 逗号分隔事实关键词（判重交集 & 判定器检索）
    ref_event_id INTEGER REFERENCES memory_events(id),
    ref_l1_id    TEXT REFERENCES memory_l1(id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
)

-- table: persona_style_stats
CREATE TABLE persona_style_stats (
    persona_uid      TEXT PRIMARY KEY NOT NULL REFERENCES personas(uid),
    sample_count     INTEGER NOT NULL,            -- 统计样本量 n_p（消息条数）
    stats_json       TEXT NOT NULL,               -- 五维统计参数（JSON，不含原文）
    baseline_version INTEGER NOT NULL DEFAULT 0,  -- 全局基线池合并版本（基线引用）
    rule_text        TEXT,                        -- 自动风格规则文本（NULL=未生成）
    rule_source      TEXT NOT NULL DEFAULT 'none',-- none / template / llm
    status           TEXT NOT NULL DEFAULT 'insufficient', -- insufficient / ready / no_significant
    updated_at       INTEGER NOT NULL             -- 更新时间（Unix 毫秒）
)

-- table: personality_traits
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
)

-- table: personas
CREATE TABLE personas (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    uid         TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL,       -- user / rama / char / anim / oc / hist
    seq         INTEGER NOT NULL,
    source      TEXT NOT NULL,       -- local / qq / wechat / telegram / manual / network
    ref_id      TEXT,                -- 来源方原始 ID
    avatar      TEXT,
    config      TEXT,                -- JSON 个性配置
    active      INTEGER NOT NULL DEFAULT 1,  -- 1=启用, 0=停用
    description TEXT,                -- 人格简介（面向用户的多角色管理描述）
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
)

-- table: privacy_consent
CREATE TABLE privacy_consent (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    provider   TEXT NOT NULL,
    base_url   TEXT NOT NULL,
    timestamp  INTEGER NOT NULL,
    persistent INTEGER NOT NULL DEFAULT 0
)

-- table: schema_meta
CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
)

-- table: session_members
CREATE TABLE session_members (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    platform_ref   TEXT NOT NULL,
    name           TEXT NOT NULL DEFAULT '',
    group_nickname TEXT,
    role           TEXT,
    first_seen_at  INTEGER NOT NULL DEFAULT 0,
    last_seen_at   INTEGER NOT NULL DEFAULT 0,
    UNIQUE(session_id, platform_ref)
)

-- table: sessions
CREATE TABLE sessions (
    id           TEXT PRIMARY KEY,        -- UUID v4
    started_at   INTEGER NOT NULL,
    ended_at     INTEGER,
    persona_uid  TEXT,                    -- Session 当前绑定对话人格
    channel      TEXT NOT NULL DEFAULT 'local',  -- local / mcp / qq / telegram
    external_ref TEXT,                    -- 外部会话标识（按 channel 解释）
    last_read_at INTEGER NOT NULL DEFAULT 0  -- 最近标记已读时间；0 = 从未标记
)

-- table: settings
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
)

-- table: sticker_assets
CREATE TABLE sticker_assets (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    file_name   TEXT NOT NULL,
    file_path   TEXT NOT NULL,             -- 资产目录内的文件名（不含目录前缀）
    md5         TEXT,
    size        INTEGER,
    description TEXT,
    tags        TEXT NOT NULL DEFAULT '[]',  -- 标签 JSON 数组
    created_at  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL DEFAULT 0
)

-- table: trait_evidence
CREATE TABLE trait_evidence (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    trait_id   INTEGER NOT NULL REFERENCES personality_traits(id),
    event_id   INTEGER NOT NULL REFERENCES memory_events(id),
    direction  TEXT NOT NULL,   -- support / contradict / neutral
    score      REAL NOT NULL,   -- -1.0..1.0
    decay      REAL NOT NULL DEFAULT 1.0,
    created_at INTEGER NOT NULL
)

-- table: utt_blocks
CREATE TABLE utt_blocks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    persona_uid  TEXT NOT NULL REFERENCES personas(uid),
    session_id   TEXT NOT NULL REFERENCES sessions(id),
    start_msg_id TEXT NOT NULL REFERENCES messages(id),
    end_msg_id   TEXT NOT NULL REFERENCES messages(id),
    block_text   TEXT NOT NULL,
    msg_count    INTEGER NOT NULL,
    time_span_ms INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
)

-- index: idx_attachments_md5
CREATE INDEX idx_attachments_md5 ON message_attachments(md5)

-- index: idx_attachments_message_id
CREATE INDEX idx_attachments_message_id ON message_attachments(message_id)

-- index: idx_attachments_status
CREATE INDEX idx_attachments_status ON message_attachments(status)

-- index: idx_behavior_rules_persona
CREATE INDEX idx_behavior_rules_persona ON behavior_rules(persona_uid)

-- index: idx_cluster_snapshots_uid_cat
CREATE INDEX idx_cluster_snapshots_uid_cat ON persona_cluster_snapshots(persona_uid, category, is_current)

-- index: idx_event_relations_from
CREATE INDEX idx_event_relations_from ON event_relations(from_id)

-- index: idx_event_relations_to
CREATE INDEX idx_event_relations_to ON event_relations(to_id)

-- index: idx_feedback_log_persona
CREATE INDEX idx_feedback_log_persona ON feedback_log(persona_uid)

-- index: idx_feedback_log_target
CREATE INDEX idx_feedback_log_target ON feedback_log(target_type, target_id)

-- index: idx_keyword_alias
CREATE INDEX idx_keyword_alias ON keyword_pool(alias_status)

-- index: idx_llm_response_cache_accessed
CREATE INDEX idx_llm_response_cache_accessed ON llm_response_cache(last_accessed_at)

-- index: idx_llm_response_cache_created
CREATE INDEX idx_llm_response_cache_created ON llm_response_cache(created_at)

-- index: idx_memory_events_persona_created
CREATE INDEX idx_memory_events_persona_created ON memory_events(persona_uid, created_at)

-- index: idx_memory_events_persona_start
CREATE INDEX idx_memory_events_persona_start ON memory_events(persona_uid, start)

-- index: idx_memory_events_share
CREATE INDEX idx_memory_events_share ON memory_events(share)

-- index: idx_memory_l1_absorbed
CREATE INDEX idx_memory_l1_absorbed ON memory_l1(absorbed)

-- index: idx_memory_l1_effective_time
CREATE INDEX idx_memory_l1_effective_time ON memory_l1(persona_uid, absorbed, COALESCE(occurred_at, created_at))

-- index: idx_memory_l1_persona
CREATE INDEX idx_memory_l1_persona ON memory_l1(persona_uid)

-- index: idx_memory_l1_persona_created
CREATE INDEX idx_memory_l1_persona_created ON memory_l1(persona_uid, created_at)

-- index: idx_memory_l1_persona_occurred
CREATE INDEX idx_memory_l1_persona_occurred ON memory_l1(persona_uid, occurred_at)

-- index: idx_memory_l1_session
CREATE INDEX idx_memory_l1_session ON memory_l1(session_id)

-- index: idx_memory_vectors_model
CREATE INDEX idx_memory_vectors_model ON memory_vectors(model, doc_type)

-- index: idx_messages_created_at
CREATE INDEX idx_messages_created_at ON messages(created_at)

-- index: idx_messages_persona
CREATE INDEX idx_messages_persona ON messages(persona_uid)

-- index: idx_messages_persona_created
CREATE INDEX idx_messages_persona_created ON messages(persona_uid, created_at)

-- index: idx_messages_platform_message_id
CREATE UNIQUE INDEX idx_messages_platform_message_id
    ON messages(session_id, platform_message_id) WHERE platform_message_id IS NOT NULL

-- index: idx_messages_sender_ref
CREATE INDEX idx_messages_sender_ref ON messages(sender_ref)

-- index: idx_messages_session
CREATE INDEX idx_messages_session ON messages(session_id)

-- index: idx_messages_session_created
CREATE INDEX idx_messages_session_created ON messages(session_id, created_at)

-- index: idx_persona_examples_uid_sel
CREATE INDEX idx_persona_examples_uid_sel ON persona_examples(persona_uid, selected)

-- index: idx_persona_facts_uid_field
CREATE INDEX idx_persona_facts_uid_field ON persona_facts(persona_uid, field, status)

-- index: idx_persona_facts_uid_status
CREATE INDEX idx_persona_facts_uid_status ON persona_facts(persona_uid, status)

-- index: idx_personality_traits_uid_layer
CREATE INDEX idx_personality_traits_uid_layer ON personality_traits(persona_uid, layer, seq)

-- index: idx_personality_traits_uid_status
CREATE INDEX idx_personality_traits_uid_status ON personality_traits(persona_uid, status)

-- index: idx_personas_kind_source_ref
CREATE UNIQUE INDEX idx_personas_kind_source_ref
    ON personas(kind, source, ref_id) WHERE ref_id IS NOT NULL

-- index: idx_privacy_consent_provider_url
CREATE INDEX idx_privacy_consent_provider_url ON privacy_consent(provider, base_url)

-- index: idx_sessions_channel_external_ref
CREATE INDEX idx_sessions_channel_external_ref
    ON sessions (channel, external_ref)

-- index: idx_sticker_assets_md5
CREATE INDEX idx_sticker_assets_md5 ON sticker_assets(md5)

-- index: idx_style_stats_status
CREATE INDEX idx_style_stats_status ON persona_style_stats(status)

-- index: idx_trait_evidence_event
CREATE INDEX idx_trait_evidence_event ON trait_evidence(event_id)

-- index: idx_trait_evidence_trait
CREATE INDEX idx_trait_evidence_trait ON trait_evidence(trait_id)

-- index: idx_utt_blocks_created_at
CREATE INDEX idx_utt_blocks_created_at ON utt_blocks(created_at)

-- index: idx_utt_blocks_persona
CREATE INDEX idx_utt_blocks_persona ON utt_blocks(persona_uid)

-- index: idx_utt_blocks_session
CREATE INDEX idx_utt_blocks_session ON utt_blocks(session_id)

-- index: idx_utt_blocks_session_created
CREATE INDEX idx_utt_blocks_session_created ON utt_blocks(session_id, created_at)
