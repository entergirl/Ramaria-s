/**
 * tests/import-view.test.js — 导入向导群聊标注回归（node --test）
 *
 * 目的:
 * - 锁定 `formatGroupMembers(members, limit)` 的展示约定：成员总数 + Top 成员列表，
 *   消息数降序（同数按名称升序）、超限时补「… 等共 N 人」；
 * - 防御：非数组 / 空数组 / 元素缺字段 / message_count 非数字不抛异常、安全降级；
 * - XSS：成员名经 escapeHtml 转义后才进入 HTML。
 *
 * 说明:
 * - import.js 是浏览器 IIFE 视图；本测试用 vm 沙箱注入最小桩加载，
 *   只调用其公开纯函数 `formatGroupMembers`，不触碰 DOM、Store 与 Tauri。
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

/** 导入视图源码（frontend/js/views/import.js） */
const IMPORT_PATH = path.resolve(__dirname, '..', 'js', 'views', 'import.js');
/** HTML 转义工具源码（frontend/js/utils/dom.js） */
const ESCAPE_PATH = path.resolve(__dirname, '..', 'js', 'utils', 'dom.js');

/**
 * 在 vm 沙箱中加载 import.js，返回其全局单例。
 *
 * 桩说明:
 * - `window`：收集 defineProperty 暴露的 `ImportView`；
 * - `setTimeout`：自动初始化在无 RamariaRouter 环境下会调度重试，桩掉避免悬空定时器；
 * - 不注入 `document` / Tauri：本测试只调用纯函数 `formatGroupMembers`。
 */
function loadImportView() {
  const escapeSource = fs.readFileSync(ESCAPE_PATH, 'utf8');
  const viewSource = fs.readFileSync(IMPORT_PATH, 'utf8');

  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    setTimeout() {},
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(escapeSource, sandbox, { filename: 'dom.js' });
  vm.runInContext(viewSource, sandbox, { filename: 'import.js' });

  const view = windowMock.ImportView;
  if (!view || typeof view.formatGroupMembers !== 'function') {
    throw new Error('import.js 未暴露 formatGroupMembers()');
  }
  return view;
}

const view = loadImportView();

/** 调用被测函数（返回字符串，跨 realm 无原型差异） */
function fmt(members, limit) {
  return view.formatGroupMembers(members, limit);
}

test('正常排序：乱序输入按消息数降序输出', () => {
  const html = fmt([
    { uid: 'u1', uin: '111', name: '张三', message_count: 5 },
    { uid: 'u2', uin: '222', name: '李四', message_count: 20 },
    { uid: 'u3', uin: '333', name: '王五', message_count: 10 },
  ]);

  assert.ok(html.includes('<strong>群聊成员:</strong> 3 人'), `应显示成员总数: ${html}`);

  const iLi = html.indexOf('李四（20 条）');
  const iWang = html.indexOf('王五（10 条）');
  const iZhang = html.indexOf('张三（5 条）');
  assert.ok(iLi >= 0 && iWang >= 0 && iZhang >= 0, `Top 成员应齐全: ${html}`);
  assert.ok(iLi < iWang && iWang < iZhang, `应按消息数降序: ${html}`);
  assert.ok(!html.includes('等共'), `未超限不应出现截断文案: ${html}`);
});

test('同消息数：按名称升序排列', () => {
  const html = fmt([
    { name: 'Banana', message_count: 1 },
    { name: 'Apple', message_count: 1 },
    { name: 'Cherry', message_count: 1 },
  ]);

  const iApple = html.indexOf('Apple（1 条）');
  const iBanana = html.indexOf('Banana（1 条）');
  const iCherry = html.indexOf('Cherry（1 条）');
  assert.ok(iApple >= 0 && iBanana >= 0 && iCherry >= 0, `成员应齐全: ${html}`);
  assert.ok(iApple < iBanana && iBanana < iCherry, `同数应按名称升序: ${html}`);
});

test('Top N 截断：默认展示前 5 并补「等共 N 人」', () => {
  const members = [];
  for (let i = 1; i <= 7; i++) {
    members.push({ name: '成员' + i, message_count: i });
  }

  const html = fmt(members);
  assert.ok(html.includes('成员7（7 条）') && html.includes('成员3（3 条）'), `应展示前 5 名: ${html}`);
  assert.ok(!html.includes('成员2（2 条）') && !html.includes('成员1（1 条）'), `第 6 名起应被截断: ${html}`);
  assert.ok(html.includes('… 等共 7 人'), `应显示截断总数: ${html}`);
});

test('Top N 截断：limit 可自定义', () => {
  const html = fmt([
    { name: 'A', message_count: 3 },
    { name: 'B', message_count: 2 },
    { name: 'C', message_count: 1 },
  ], 2);

  assert.ok(html.includes('A（3 条）') && html.includes('B（2 条）'), `应展示前 2 名: ${html}`);
  assert.ok(!html.includes('C（1 条）'), `第 3 名应被截断: ${html}`);
  assert.ok(html.includes('… 等共 3 人'), `应显示截断总数: ${html}`);
});

test('limit 非法值回退为默认 5', () => {
  const members = [];
  for (let i = 1; i <= 6; i++) {
    members.push({ name: 'M' + i, message_count: i });
  }

  const html = fmt(members, 0);
  assert.ok(html.includes('M6（6 条）') && html.includes('M2（2 条）'), `应按默认 5 展示: ${html}`);
  assert.ok(!html.includes('M1（1 条）'), `第 6 名应被截断: ${html}`);
});

test('空数组：返回空串', () => {
  assert.equal(fmt([]), '');
});

test('防御：非数组输入返回空串且不抛异常', () => {
  assert.equal(fmt(null), '');
  assert.equal(fmt(undefined), '');
  assert.equal(fmt('not-an-array'), '');
  assert.equal(fmt({ members: [] }), '');
  assert.equal(fmt(42), '');
});

test('防御：无效元素跳过、缺字段回退、count 非数字按 0', () => {
  const html = fmt([
    null,
    'junk',
    { uin: '123456', message_count: 3 },       // 缺 name → 回退 uin
    { message_count: 2 },                       // name / uin 均缺 → 「未知」
    { name: '计数异常', message_count: '99' },   // count 非数字 → 按 0 计数
  ]);

  assert.ok(html.includes('<strong>群聊成员:</strong> 3 人'), `仅统计有效元素: ${html}`);
  assert.ok(html.includes('123456（3 条）'), `缺 name 应回退 uin: ${html}`);
  assert.ok(html.includes('未知（2 条）'), `缺名应回退「未知」: ${html}`);
  assert.ok(html.includes('计数异常（0 条）'), `非数字 count 应按 0 计数: ${html}`);
});

test('防御：全部为无效元素返回空串', () => {
  assert.equal(fmt([null, 42, 'x']), '');
});

test('HTML 转义：成员名中的标签被转义', () => {
  const html = fmt([{ name: '<script>alert(1)</script>', message_count: 1 }]);

  assert.ok(html.includes('&lt;script&gt;alert(1)&lt;/script&gt;'), `成员名应转义: ${html}`);
  assert.ok(!html.includes('<script>alert(1)</script>'), `不得输出未转义标签: ${html}`);
});

test('HTML 转义：引号与 & 一并转义', () => {
  const html = fmt([{ name: 'A & "B" <C>', message_count: 1 }]);
  assert.ok(html.includes('A &amp; &quot;B&quot; &lt;C&gt;'), `特殊字符应转义: ${html}`);
});
