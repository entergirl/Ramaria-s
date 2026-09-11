/**
 * js/utils/probe.js — probe 产物解析（纯函数，零 DOM / 零 IPC）
 *
 * 职责:
 * - 将 ramaria-cli `probe run/evaluate/report` 产出的 JSON 提炼为调试面板可渲染的摘要。
 * - 仅做只读解析与规整，不触发任何后端运行、不修改产物。
 * - 所有函数为确定性纯函数，供前端 `debug.js` 调用并经 node --test 回归。
 *
 * 设计特点:
 * - 通过 RamariaProbe 全局单例访问（与其它 utils 一致）。
 * - 产物结构存在多代差异（run / evaluate / report 各有 variants），
 *   解析采取"特征键探测 + 数值提取"的宽松策略，任何未知形态都安全降级为空态。
 * - 返回对象仅含可 JSON 序列化的标量/数组，便于视图直接渲染。
 *
 * 依赖: 无
 */

var RamariaProbe = (function () {
    'use strict';

 // =========================================================
 // 产物类型识别
 // =========================================================

 /**
 * 识别产物 JSON 的类型。
 *
 * 参数:
 * - `value`: 产物 JSON（已解析为对象）
 *
 * 返回:
 * - "report"（消融/报告） | "evaluate"（评分） | "run"（运行） | "unknown"
 *
 * 判定顺序:
 * - report: 含 recommendation / knowledge_quality / auxiliary / ablation 之一
 * - evaluate: 含 judge_used 且 variants 元素含 items
 * - run: 含 dataset_seed 且 variants 元素含 runs
 * - 其余 unknown
 */
    function detectKind(value) {
        if (!value || typeof value !== 'object' || Array.isArray(value)) return 'unknown';
        if (hasAny(value, ['recommendation', 'knowledge_quality', 'auxiliary', 'ablation'])) {
            return 'report';
        }
        if (value.judge_used !== undefined || value.judge_used === true || value.judge_used === false) {
            return 'evaluate';
        }
        if (hasAny(value, ['dataset_seed', 'generated_at']) && value.variants !== undefined) {
            return 'run';
        }
        return 'unknown';
    }

 /**
 * 判断对象是否含给定键中的任意一个（键存在且值非 null）。
 */
    function hasAny(obj, keys) {
        for (var i = 0; i < keys.length; i++) {
            if (obj[keys[i]] !== undefined && obj[keys[i]] !== null) return true;
        }
        return false;
    }

 // =========================================================
 // 元信息提取
 // =========================================================

 /**
 * 提取产物公共元信息（可缺失字段安全回退）。
 */
    function extractMeta(value) {
        var v = value && typeof value === 'object' ? value : {};
        var meta = {
            kind: detectKind(v),
            personaUid: firstStr(v, ['persona_uid', 'personaUid']),
            datasetSeed: firstStr(v, ['dataset_seed', 'datasetSeed']),
            generatedAt: firstStr(v, ['generated_at', 'generatedAt']),
            judgeUsed: boolOrNull(v.judge_used),
            embeddingUsed: boolOrNull(v.embedding_used),
            resultsFile: firstStr(v, ['results_file', 'resultsFile', 'evaluation_file', 'evaluationFile']),
        };
        return meta;
    }

 /**
 * 取首个存在且非空字符串的键值。
 */
    function firstStr(obj, keys) {
        for (var i = 0; i < keys.length; i++) {
            var val = obj[keys[i]];
            if (typeof val === 'string' && val !== '') return val;
            if (typeof val === 'number' && !isNaN(val)) return String(val);
        }
        return null;
    }

    function boolOrNull(val) {
        return typeof val === 'boolean' ? val : null;
    }

 // =========================================================
 // variants 提取
 // =========================================================

 /**
 * 从产物中提取档位行。
 *
 * 参数:
 * - `value`: 产物 JSON
 *
 * 返回:
 * - [{ name, scores: [{label, value}], extra }]
 *
 * 说明:
 * - variants 可能是数组，或按档位名索引的对象。
 * - 每个元素可能带 fact/tone/emotion/mean 等标量分数，或嵌套 dimension_scores 聚合。
 */
    function extractVariants(value) {
        var v = value && typeof value === 'object' ? value : {};
        var raw = v.variants;
        if (raw === undefined || raw === null) return [];

        var list = Array.isArray(raw)
            ? raw
            : Object.keys(raw).map(function (key) {
                // 按档位名索引的对象：把键并入元素作为显示名兜底（元素自身字段优先）
                var item = raw[key];
                if (!item || typeof item !== 'object') return { name: key };
                var merged = {};
                Object.keys(item).forEach(function (k) { merged[k] = item[k]; });
                if (merged.name === undefined) merged.name = key;
                return merged;
            });

        var rows = [];
        for (var i = 0; i < list.length; i++) {
            var item = list[i];
            if (!item || typeof item !== 'object') continue;
            rows.push(extractVariantRow(item, i));
        }
        return rows;
    }

 /**
 * 提炼单条档位。
 */
    function extractVariantRow(item, index) {
        var name = variantName(item, index);
        var scores = extractScores(item);
        var params = item.params && typeof item.params === 'object' ? item.params : null;
        var ablation = params && params.ablation ? String(params.ablation) : null;
        return {
            name: name,
            ablation: ablation,
            // 注入闸门语义（人读提示，服务 M8 排查）
            gateText: gateDescription(ablation),
            scores: scores,
            rawItems: countItems(item),
        };
    }

 /**
 * 档位名 → 注入闸门语义描述（探针消融口径，见 ablation-profile-mapping）。
 *
 * 返回:
 * - 已知档位返回中文说明；未知返回 null。
 */
    function gateDescription(ablation) {
        if (!ablation || typeof ablation !== 'string') return null;
        if (ablation === 'B0') return '无注入基线（全部关闭）';
        if (ablation === 'B1') return '完整基线（RAG + 四层）';
        if (ablation === 'F0') return '移除专属层（仅 RAG 基座）';
        if (/^F[1-4]$/.test(ablation)) return '移除对照（F 组单层移除）';
        if (ablation.indexOf('I_') === 0) {
            return '净增量：B1 基座 + ' + ablation.slice(2) + ' 专属层';
        }
        if (ablation.indexOf('S_') === 0) {
            return '替代对照：去 RAG 仅保留 ' + ablation.slice(2) + ' 层';
        }
        return null;
    }

 /**
 * 档位显示名（宽松回退）。
 */
    function variantName(item, index) {
        var direct = firstStr(item, ['variant_id', 'variant', 'variantId', 'id', 'name']);
        if (direct) return direct;
        if (item.params && item.params.ablation) return String(item.params.ablation);
        return '档位 ' + (index + 1);
    }

 /**
 * 统计档位内明细条数（items/runs/failed 兼容）。
 */
    function countItems(item) {
        if (item.items !== undefined) return item.items;
        if (item.runs !== undefined) return item.runs;
        if (typeof item.failed_count === 'number') return item.failed_count;
        return null;
    }

 /**
 * 提取档位内的标量数值评分。
 *
 * 返回:
 * - [{label, value}]，label 已中文归一。
 */
    function extractScores(item) {
        var out = [];
        // 常见整档评分键 → 中文标签
        var LABEL_MAP = {
            fact_score: '事实',
            fact_score_norm: '事实(归一)',
            fact_score_point: '事实(事实点)',
            tone_score: '语气',
            emotion_score: '情绪',
            fact_mean: '事实均值',
            tone_mean: '语气均值',
            emotion_mean: '情绪均值',
            overall: '综合',
            mean: '均值',
            score: '得分',
        };

        var KEY_ORDER = ['fact_score', 'fact_score_norm', 'fact_score_point',
            'fact_mean', 'tone_score', 'tone_mean',
            'emotion_score', 'emotion_mean', 'overall', 'score', 'mean'];

        for (var i = 0; i < KEY_ORDER.length; i++) {
            var key = KEY_ORDER[i];
            if (item[key] !== undefined && typeof item[key] === 'number' && isFinite(item[key])) {
                out.push({ label: LABEL_MAP[key] || key, value: round(item[key]) });
            }
        }

        // 若存在嵌套 dimension_scores 聚合（evaluate --repeat），并入主列表
        var dims = item.dimension_scores;
        if (dims && typeof dims === 'object' && !Array.isArray(dims)) {
            var DIM_LABELS = {
                fact: '事实',
                fact_norm: '事实(归一)',
                fact_point: '事实(事实点)',
                tone: '语气',
                emotion: '情绪',
            };
            Object.keys(dims).forEach(function (key) {
                var d = dims[key];
                if (!d || typeof d !== 'object') return;
                if (typeof d.mean === 'number' && isFinite(d.mean)) {
                    var dimLabel = DIM_LABELS[key] || key;
                    out.push({
                        label: dimLabel + '（逐轮）',
                        value: round(d.mean),
                        detail: 'n=' + (d.n !== undefined ? d.n : '?') +
                            (typeof d.ci95_low === 'number' ? ' CI[' + round(d.ci95_low) + ',' + round(d.ci95_high) + ']' : ''),
                    });
                }
            });
        }

        return out;
    }

    function round(x) {
        return Math.round(x * 1000) / 1000;
    }

 // =========================================================
 // 汇总入口
 // =========================================================

 /**
 * 汇总单个产物 JSON 为调试面板渲染对象。
 *
 * 参数:
 * - `value`: 已解析的产物对象
 *
 * 返回:
 * - { kind, meta, variants, auxiliary, hasContent }
 */
    function summarize(value) {
        var meta = extractMeta(value);
        var variants = extractVariants(value);
        var auxiliary = normalizeAuxiliary(value && value.auxiliary);
        var hasContent = variants.length > 0 || !!meta.personaUid || auxiliary !== null;
        return {
            kind: meta.kind,
            meta: meta,
            variants: variants,
            auxiliary: auxiliary,
            hasContent: hasContent,
        };
    }

 /**
 * 归一化辅助指标四件套（report 产物）。
 */
    function normalizeAuxiliary(aux) {
        if (!aux || typeof aux !== 'object') return null;
        var items = [];
        if (typeof aux.evidence_traceability_rate === 'number') {
            items.push({ label: '证据链可追溯率', value: aux.evidence_traceability_rate });
        }
        if (typeof aux.behavior_rule_hit_rate === 'number') {
            items.push({ label: '行为规则命中率', value: aux.behavior_rule_hit_rate });
        }
        if (typeof aux.situation_route_misuse_rate === 'number') {
            items.push({ label: '情境路由误用率', value: aux.situation_route_misuse_rate });
        }
        if (typeof aux.profile_regression_output_stability === 'number') {
            items.push({ label: '画像回归稳定性', value: aux.profile_regression_output_stability });
        }
        return items.length > 0 ? items : null;
    }

 // =========================================================
 // 导出
 // =========================================================

    return {
        detectKind: detectKind,
        summarize: summarize,
        extractVariants: extractVariants,
        normalizeAuxiliary: normalizeAuxiliary,
        gateDescription: gateDescription,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaProbe', {
    value: RamariaProbe,
    writable: false,
    configurable: false,
});
