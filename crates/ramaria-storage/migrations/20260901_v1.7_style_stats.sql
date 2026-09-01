-- =========================================================
-- Ramaria v1.7 增量迁移 —— persona_style_stats（表达层 A3）
--
-- 说明:
-- - 增量迁移（只增不删，非破坏性变更）：旧库无需重建。
-- - 新增风格统计表：按 persona 单行 upsert，存五维统计参数、
--   样本量、基线引用与自动风格规则文本。
-- - 隐私红线：stats_json 仅存统计参数（频率/计数/分布），
--   不含原文消息文本；规则文本为自动生成的风格描述。
-- =========================================================

CREATE TABLE persona_style_stats (
    persona_uid      TEXT PRIMARY KEY NOT NULL REFERENCES personas(uid),
    sample_count     INTEGER NOT NULL,            -- 统计样本量 n_p（消息条数）
    stats_json       TEXT NOT NULL,               -- 五维统计参数（JSON，不含原文）
    baseline_version INTEGER NOT NULL DEFAULT 0,  -- 全局基线池合并版本（基线引用）
    rule_text        TEXT,                        -- 自动风格规则文本（NULL=未生成）
    rule_source      TEXT NOT NULL DEFAULT 'none',-- none / template / llm
    status           TEXT NOT NULL DEFAULT 'insufficient', -- insufficient / ready / no_significant
    updated_at       INTEGER NOT NULL             -- 更新时间（Unix 毫秒）
);
CREATE INDEX idx_style_stats_status ON persona_style_stats(status);
