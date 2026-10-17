/**
 * tests/persona-import.test.js — 人格预设导入展示口径回归（node --test）
 *
 * 目的:
 * - 锁定 `formatImportSummary(outcome)` 的结果提示口径：uid+名称+模式（新建/合并）、
 *   四类写入计数、失败条数、拆解状态（未启用 / 已执行 / 三种跳过原因）；
 * - 锁定 `buildImportPreviewHtml(preview)` 的预览形态：预览文本进 `<pre>` 且经
 *   `RamariaEscape.escapeHtml` 转义（XSS 防护）、缺字段安全降级为空文本；
 * - 锁定视图接线：导入链路必须调用 `RamariaApi.persona.previewImport` /
 *   `RamariaApi.persona.importPreset`（防静默改名导致结果提示链路断裂）。
 *
 * 说明:
 * - persona.js 是浏览器 IIFE 视图；本测试用 vm 沙箱注入最小桩加载，
 *   只调用其公开纯函数，不触碰 DOM、Store 与 Tauri。
 * - RamariaEscape 从 utils/dom.js 原样加载进同一沙箱，转义断言针对真实实现。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** 人格视图源码（frontend/js/views/persona.js） */
const PERSONA_PATH = path.resolve(__dirname, '..', 'js', 'views', 'persona.js');
/** HTML 转义工具源码（frontend/js/utils/dom.js） */
const ESCAPE_PATH = path.resolve(__dirname, '..', 'js', 'utils', 'dom.js');

/**
 * 在 vm 沙箱中加载 persona.js，返回其全局单例与源码文本。
 *
 * 桩说明:
 * - `window`：收集 defineProperty 暴露的 `RamariaPersonaView`；
 * - `setInterval` / `setTimeout`：自动初始化在无 RamariaRouter 环境下会轮询重试，桩掉避免悬空定时器；
 * - 不注入 `document` / Tauri：本测试只调用纯函数。
 */
function loadPersonaView() {
  const escapeSource = fs.readFileSync(ESCAPE_PATH, 'utf8');
  const viewSource = fs.readFileSync(PERSONA_PATH, 'utf8');

  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    setTimeout() {},
    setInterval() {
      return 0;
    },
    clearInterval() {},
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(escapeSource, sandbox, { filename: 'dom.js' });
  vm.runInContext(viewSource, sandbox, { filename: 'persona.js' });

  const view = windowMock.RamariaPersonaView;
  if (!view || typeof view.formatImportSummary !== 'function') {
    throw new Error('persona.js 未暴露 formatImportSummary()');
  }
  return { view, viewSource };
}

const { view, viewSource } = loadPersonaView();

// =========================================================
// formatImportSummary — 结果提示口径
// =========================================================

test('新建模式：uid / 名称 / 四类计数齐全', () => {
  const summary = view.formatImportSummary({
    uid: 'char-0007',
    name: '黎杋枫',
    mode: 'create',
    written: { traits: 3, facts: 2, examples: 1, behavior_rules: 1, failed: 0 },
    decompose: { requested: false, executed: false, fact_count: 0, trait_count: 0 },
  });

  assert.ok(summary.includes('char-0007'), `应含 uid: ${summary}`);
  assert.ok(summary.includes('「黎杋枫」'), `应含人格名: ${summary}`);
  assert.ok(summary.includes('（新建）'), `应含模式: ${summary}`);
  assert.ok(
    summary.includes('traits 3 / facts 2 / examples 1 / behavior_rules 1'),
    `应含四类计数: ${summary}`
  );
  assert.ok(summary.includes('拆解：未启用'), `未请求拆解应显示未启用: ${summary}`);
  assert.ok(!summary.includes('失败'), `零失败不应出现失败文案: ${summary}`);
});

test('合并模式：显示合并并给出失败条数', () => {
  const summary = view.formatImportSummary({
    uid: 'char-0008',
    name: '预览人格',
    mode: 'merge',
    written: { traits: 0, facts: 1, examples: 0, behavior_rules: 0, failed: 2 },
    decompose: { requested: true, executed: false, skipped_reason: 'privacy_unconfirmed' },
  });

  assert.ok(summary.includes('（合并）'), `应含合并模式: ${summary}`);
  assert.ok(summary.includes('失败 2 条'), `应含失败条数: ${summary}`);
  assert.ok(summary.includes('拆解：已跳过（隐私未确认）'), `应含跳过原因: ${summary}`);
});

test('拆解三态：已执行 / LLM 失败 / 无输入', () => {
  const executed = view.formatImportSummary({
    uid: 'char-0009',
    mode: 'create',
    written: {},
    decompose: { requested: true, executed: true, trait_count: 2, fact_count: 1 },
  });
  assert.ok(executed.includes('拆解：已执行（traits 2 / facts 1）'), executed);

  const failed = view.formatImportSummary({
    uid: 'char-0009',
    mode: 'create',
    written: {},
    decompose: { requested: true, executed: false, skipped_reason: 'llm_failed' },
  });
  assert.ok(failed.includes('拆解：已跳过（LLM 调用失败）'), failed);

  const noInput = view.formatImportSummary({
    uid: 'char-0009',
    mode: 'create',
    written: {},
    decompose: { requested: true, executed: false, skipped_reason: 'no_input' },
  });
  assert.ok(noInput.includes('拆解：已跳过（无拆解输入）'), noInput);
});

test('缺字段安全降级：null / 空对象不抛异常', () => {
  assert.doesNotThrow(() => view.formatImportSummary(null));
  assert.doesNotThrow(() => view.formatImportSummary({}));

  const summary = view.formatImportSummary({});
  assert.ok(summary.includes('（未知）'), `uid 缺失应显示占位: ${summary}`);
  assert.ok(summary.includes('traits 0'), `计数缺失应按 0: ${summary}`);
});

// =========================================================
// buildImportPreviewHtml — 预览弹窗正文
// =========================================================

test('预览正文：文本进 pre 且 HTML 转义（XSS 防护）', () => {
  const html = view.buildImportPreviewHtml({
    text: '<img src=x onerror=alert(1)>预设预览体',
  });

  assert.ok(html.includes('class="persona-import-preview"'), `应含预览容器: ${html}`);
  assert.ok(html.includes('预设预览体'), `应保留预览文本: ${html}`);
  assert.ok(!html.includes('<img'), `标签应被转义: ${html}`);
  assert.ok(html.includes('&lt;img'), `应输出转义实体: ${html}`);
});

test('预览正文：缺字段安全降级为空 pre', () => {
  const html = view.buildImportPreviewHtml(null);
  assert.ok(html.includes('<pre class="persona-import-preview"></pre>'), html);
  assert.ok(html.includes('只增不改'), `应含写入语义提示: ${html}`);

  const html2 = view.buildImportPreviewHtml({ text: 123 });
  assert.ok(html2.includes('<pre class="persona-import-preview"></pre>'), html2);
});

// =========================================================
// 视图接线
// =========================================================

test('导入链路调用 api.persona 的预览与导入封装', () => {
  assert.ok(
    viewSource.includes('RamariaApi.persona.previewImport('),
    '视图应调用 RamariaApi.persona.previewImport()'
  );
  assert.ok(
    viewSource.includes('RamariaApi.persona.importPreset('),
    '视图应调用 RamariaApi.persona.importPreset()'
  );
});
