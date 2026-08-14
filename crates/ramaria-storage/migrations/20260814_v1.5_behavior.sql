-- =========================================================
-- Ramaria v1.5 Migration —— 行为模型学习与驱动（M5，D + H1）
--
-- 内容:
-- 1. behavior_rules 表：行为规则（算法说明书 v3.1 §4.1）。
--    情境侧 situation（关键词集 + 簇中心向量 + valence 分布）与行为侧
--    reaction（规则文本）/ params（JSON 参数）/ avoid（JSON）均以 JSON 列存储；
--    evidence 只存 事件 id + 权重（原文经 memory_events 二次查询，原文不落此表）。
--    source = auto | manual（Manual 优先级高于 Auto）；enabled 控制是否参与路由。
--    reaction 可为 NULL = 候选规则（仅参数注入，D4 质控降级轨道）。
-- 2. feedback_log 表：反馈日志（v3.1 §9.4，H1 S1 写入；S2/S3 v1.7 复用同表只增不删）。
--    signal_type = edit | disable（S1 强信号，weight=1.0）；detail 存编辑前后快照 JSON。
--
-- 幂等性说明:
-- - 全部使用 CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS，
--   整个 SQL 文件可安全重复执行。
-- - 本 migration 不修改任何既有表（增量迁移，见 v1.5 破坏性变更声明）。
--
-- 命名说明:
-- - 文件版本号取 20260814（而非任务清单字面 20260810）：sqlx 以文件名 `_` 前
--   数字为 migration version，20260810 已用于 `20260810_v1.5_cache.sql`，重复会冲突。
-- =========================================================

-- ---------------------------------------------------------
-- 1. behavior_rules 表
-- ---------------------------------------------------------

CREATE TABLE IF NOT EXISTS behavior_rules (
    -- 规则 id（INTEGER AUTOINCREMENT）
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 所属 persona（规则按 persona 严格隔离）
    persona_uid TEXT NOT NULL,
    -- 情境侧特征 JSON（BehaviorSituation：关键词集/簇中心向量/valence 分布/presentation 分布）
    situation TEXT NOT NULL,
    -- 规则文本（NULL = 候选规则，仅参数注入，D4 质控降级轨道）
    reaction TEXT,
    -- 结构化参数 JSON（BehaviorParams：情感强度/主动程度/详细度/正式度）
    params TEXT NOT NULL,
    -- 禁忌/注意列表 JSON（字符串数组）
    avoid TEXT NOT NULL DEFAULT '[]',
    -- 证据链 JSON（BehaviorEvidence：事件 id + 权重）
    evidence TEXT NOT NULL DEFAULT '[]',
    -- 置信度 0.0..1.0（证据量 × 一致性）
    confidence REAL NOT NULL DEFAULT 0,
    -- 稳定性 0.0..1.0（跨时间一致性）
    stability REAL NOT NULL DEFAULT 0,
    -- 来源: auto | manual（manual 优先级高于 auto，作为聚类强锚点）
    source TEXT NOT NULL DEFAULT 'auto',
    -- 是否启用（参与路由；disabled 规则管理端可见但不注入）
    enabled INTEGER NOT NULL DEFAULT 1,
    -- 创建时间（epoch ms）
    created_at INTEGER NOT NULL,
    -- 最近更新时间（epoch ms）
    updated_at INTEGER NOT NULL
);

-- 按 persona 查询（list 命令）索引。
CREATE INDEX IF NOT EXISTS idx_behavior_rules_persona
    ON behavior_rules(persona_uid);

-- ---------------------------------------------------------
-- 2. feedback_log 表（v3.1 §9.4，H1 S1；S2/S3 v1.7 复用）
-- ---------------------------------------------------------

CREATE TABLE IF NOT EXISTS feedback_log (
    -- 日志 id（INTEGER AUTOINCREMENT）
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 所属 persona
    persona_uid TEXT NOT NULL,
    -- 标的类型: behavior_rule | persona_fact | personality_trait
    target_type TEXT NOT NULL,
    -- 标的 id（字符串形式）
    target_id TEXT NOT NULL,
    -- 信号类型: edit | disable（S1）；correction | continue（S2/S3，v1.7）
    signal_type TEXT NOT NULL,
    -- 信号权重（S1 = 1.0；S2 = 0.6；S3 = 0.2）
    weight REAL NOT NULL DEFAULT 1.0,
    -- 干预发生的会话（可选，审计关联）
    session_id TEXT,
    -- 编辑前后快照 JSON（可选；只存规则字段快照，不存对话原文）
    detail TEXT,
    -- 记录时间（epoch ms）
    created_at INTEGER NOT NULL
);

-- 按 persona 查询索引（审计/证据链展示）。
CREATE INDEX IF NOT EXISTS idx_feedback_log_persona
    ON feedback_log(persona_uid);

-- 按标的反查索引（某条规则的全部干预历史）。
CREATE INDEX IF NOT EXISTS idx_feedback_log_target
    ON feedback_log(target_type, target_id);
