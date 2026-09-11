/**
 * tests/probe.test.js — RamariaProbe 产物解析纯函数回归（node --test）
 *
 * 覆盖:
 * - 类型识别（report / evaluate / run / unknown）
 * - variants 提取（数组与按档位名索引两种形态）
 * - 常见评分键与 dimension_scores 聚合提取
 * - 辅助指标四件套归一化
 * - 空态 / 畸形输入安全回退
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadUtil } = require('./helpers/load-util.js');

const probe = loadUtil('probe.js');

// vm 跨 realm 对象原型不同，deepStrictEqual 会因原型差异失败 → 用 JSON 序列化比较
function sameJson(a, b) {
  assert.equal(JSON.stringify(a), JSON.stringify(b));
}

// ---- 类型识别 ----

test('detectKind: report 识别', () => {
  const v = { recommendation: {}, knowledge_quality: {}, auxiliary: {} };
  assert.equal(probe.detectKind(v), 'report');
});

test('detectKind: evaluate 识别', () => {
  const v = { judge_used: false, variants: [{ items: [] }] };
  assert.equal(probe.detectKind(v), 'evaluate');
});

test('detectKind: run 识别', () => {
  const v = { dataset_seed: 1, generated_at: 'x', variants: [{ runs: [] }] };
  assert.equal(probe.detectKind(v), 'run');
});

test('detectKind: unknown / 空态安全回退', () => {
  assert.equal(probe.detectKind(null), 'unknown');
  assert.equal(probe.detectKind('str'), 'unknown');
  assert.equal(probe.detectKind([1, 2]), 'unknown');
  assert.equal(probe.detectKind({ foo: 1 }), 'unknown');
});

// ---- summarize ----

test('summarize: report 形态提取元信息与档位', () => {
  const v = {
    persona_uid: 'rama-0001',
    judge_used: true,
    generated_at: '2026-09-01T00:00:00Z',
    variants: [
      { variant_id: 'B1', fact_score: 0.8, tone_score: 3.2, params: { ablation: 'B1' } },
      { variant_id: 'F0', fact_score: 0.5, params: { ablation: 'F0' } },
    ],
    auxiliary: { evidence_traceability_rate: 0.9, profile_regression_output_stability: 0.2 },
  };
  const s = probe.summarize(v);
  assert.equal(s.kind, 'report');
  assert.equal(s.meta.personaUid, 'rama-0001');
  assert.equal(s.meta.judgeUsed, true);
  assert.equal(s.variants.length, 2);
  assert.equal(s.variants[0].name, 'B1');
  assert.equal(s.variants[0].ablation, 'B1');
  assert.equal(s.variants[0].gateText, '完整基线（RAG + 四层）');
  sameJson(s.variants[1].scores, [{ label: '事实', value: 0.5 }]);
  assert.equal(s.auxiliary.length, 2);
  assert.equal(s.hasContent, true);
});

test('gateDescription: 已知/未知档位语义', () => {
  assert.equal(probe.gateDescription('I_behavior'), '净增量：B1 基座 + behavior 专属层');
  assert.equal(probe.gateDescription('S_expression'), '替代对照：去 RAG 仅保留 expression 层');
  assert.equal(probe.gateDescription('F2'), '移除对照（F 组单层移除）');
  assert.equal(probe.gateDescription('F0'), '移除专属层（仅 RAG 基座）');
  assert.equal(probe.gateDescription('B0'), '无注入基线（全部关闭）');
  assert.equal(probe.gateDescription('whatever'), null);
  assert.equal(probe.gateDescription(null), null);
});

test('summarize: evaluate 形态含 dimension_scores 聚合', () => {
  const v = {
    judge_used: true,
    variants: {
      I_behavior: {
        fact_mean: 0.6,
        dimension_scores: {
          fact: { mean: 0.61, n: 3, ci95_low: 0.5, ci95_high: 0.72 },
          tone: { mean: 3.0, n: 3, ci95_low: 2.5, ci95_high: 3.5 },
        },
      },
    },
  };
  const s = probe.summarize(v);
  assert.equal(s.kind, 'evaluate');
  assert.equal(s.variants.length, 1);
  assert.equal(s.variants[0].name, 'I_behavior');
  // fact_mean 与 dimension 聚合都并入
  const labels = s.variants[0].scores.map((x) => x.label);
  assert.ok(labels.includes('事实均值'), `实际标签: ${labels.join(',')}`);
  assert.ok(labels.includes('事实（逐轮）'), `实际标签: ${labels.join(',')}`);
});

test('summarize: 事实维重算口径（长度归一 / 事实点）标签可见', () => {
  const v = {
    judge_used: false,
    variants: [
      {
        variant_id: 'B1',
        fact_score: 0.44,
        fact_score_norm: 0.52,
        fact_score_point: 0.59,
        dimension_scores: {
          fact_norm: { mean: 0.52, n: 2 },
          fact_point: { mean: 0.59, n: 2 },
        },
      },
    ],
  };
  const s = probe.summarize(v);
  const labels = s.variants[0].scores.map((x) => x.label);
  assert.ok(labels.includes('事实(归一)'), `实际标签: ${labels.join(',')}`);
  assert.ok(labels.includes('事实(事实点)'), `实际标签: ${labels.join(',')}`);
  assert.ok(labels.includes('事实(归一)（逐轮）'), `实际标签: ${labels.join(',')}`);
  assert.ok(labels.includes('事实(事实点)（逐轮）'), `实际标签: ${labels.join(',')}`);
});

test('summarize: run 形态可识别并返回空态', () => {
  const s = probe.summarize({ dataset_seed: 42, variants: [] });
  assert.equal(s.kind, 'run');
  assert.equal(s.variants.length, 0);
  // 无 persona / variants / auxiliary → hasContent=false
  const empty = probe.summarize({});
  assert.equal(empty.hasContent, false);
});

test('summarize: 畸形输入不抛异常', () => {
  const s = probe.summarize(null);
  assert.equal(s.kind, 'unknown');
  assert.equal(s.variants.length, 0);
  assert.equal(s.hasContent, false);
});

// ---- normalizeAuxiliary ----

test('normalizeAuxiliary: 只收四件套数值项', () => {
  const items = probe.normalizeAuxiliary({
    evidence_traceability_rate: 0.9,
    behavior_rule_hit_rate: 0.5,
    situation_route_misuse_rate: 0.1,
    profile_regression_output_stability: 0.2,
    annotation: '备注不应进入指标列表',
  });
  assert.equal(items.length, 4);
  assert.equal(items[0].label, '证据链可追溯率');
});

test('normalizeAuxiliary: 空/畸形返回 null', () => {
  assert.equal(probe.normalizeAuxiliary(null), null);
  assert.equal(probe.normalizeAuxiliary({}), null);
});
