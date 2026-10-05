/**
 * tests/settings-proactive-personas.test.js — 设置页「主动消息名单」回归（node --test）
 *
 * 覆盖:
 * - 状态文案四态（自动开启 / 未解锁 / 已开启（手动）/ 已关闭）+ user 行说明
 * - 行视图模型：mode 归一化、user 行禁用、顺序保持、字段透传
 * - 切换成功：调用写命令（uid/mode 正确）、更新前值与状态文案、成功提示
 * - 冷人格手动强开：素材提示（不阻塞保存）
 * - 切换失败：控件回滚到切换前值、错误提示、状态文案不回显成功
 * - 加载失败：错误态提示（不回显旧值）；空名单：空态文案
 *
 * 说明:
 * - settings.js 是浏览器 IIFE；本测试用 vm 注入 window/console/Router 桩加载，
 *   再调用 `RamariaSettingsView` 暴露的纯函数与处理器，不触发真实渲染流程；
 * - vm 沙箱返回值的原型属于沙箱 realm，断言前用 JSON 往返归一（与既有测试同一注意事项）；
 * - 处理器测试用最小 DOM 桩：document.getElementById 返回状态文案占位或列表容器。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** 设置页视图源码（frontend/js/views/settings.js） */
const SETTINGS_PATH = path.resolve(__dirname, '..', 'js', 'views', 'settings.js');

/** JSON 往返归一（vm realm 对象 → 宿主 realm） */
function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

/** 最小 HTML 转义（与 js/utils/dom.js 口径一致的测试桩） */
function escapeHtmlStub(str) {
  if (str === undefined || str === null) return '';
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/**
 * 在 vm 沙箱中加载 settings.js，返回视图单例与调用记录。
 *
 * 参数:
 * - `opts.listResult`: listPersonas 返回的名单数组；
 * - `opts.listFails`: 非空时 listPersonas 抛出该消息；
 * - `opts.setFails`: 非空时 setPersona 抛出该消息；
 * - `opts.elements`: document.getElementById 的 id → 元素桩映射。
 *
 * 返回:
 * - `view`: RamariaSettingsView；
 * - `calls`: { setCalls, toasts, listCalls }。
 */
function loadSettingsView(opts) {
  const options = opts || {};
  const source = fs.readFileSync(SETTINGS_PATH, 'utf8');

  const calls = { setCalls: [], toasts: [], listCalls: 0 };
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    RamariaRouter: {
      registerHook() {
        return function () {};
      },
      getCurrentView() {
        return 'chat';
      },
      setContentActions() {},
    },
    RamariaEscape: { escapeHtml: escapeHtmlStub },
    RamariaToast: {
      show(type, title, detail) {
        calls.toasts.push({ type, title, detail });
      },
    },
    RamariaApi: {
      proactive: {
        async listPersonas() {
          calls.listCalls += 1;
          if (options.listFails) throw new Error(options.listFails);
          return options.listResult || [];
        },
        async setPersona(uid, mode) {
          calls.setCalls.push({ uid, mode });
          if (options.setFails) throw new Error(options.setFails);
          return 'ok';
        },
      },
    },
    document: {
      getElementById(id) {
        const elements = options.elements || {};
        return elements[id] || null;
      },
    },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'settings.js' });

  const view = windowMock.RamariaSettingsView;
  if (!view || typeof view.buildProactivePersonaRows !== 'function') {
    throw new Error('settings.js 未暴露名单回归接口');
  }
  return { view, calls };
}

/** 构造三态控件桩 */
function selectStub(fields) {
  return {
    value: fields.value,
    disabled: false,
    dataset: {
      uid: fields.uid,
      prevMode: fields.prevMode,
      hasDialogue: fields.hasDialogue,
      kind: fields.kind,
    },
  };
}

// =========================================================
// 状态文案与行视图模型（纯函数）
// =========================================================

test('状态文案四态与 user 行说明', () => {
  const { view } = loadSettingsView();

  assert.equal(view.proactivePersonaStatusText('auto', true, 'char'), '自动开启（已对话过）');
  assert.equal(view.proactivePersonaStatusText('auto', false, 'char'), '未解锁（对话一次后自动开启）');
  assert.equal(view.proactivePersonaStatusText('on', false, 'char'), '已开启（手动）');
  assert.equal(view.proactivePersonaStatusText('off', true, 'char'), '已关闭');
  assert.equal(view.proactivePersonaStatusText('auto', true, 'user'), '用户人格不参与主动对话');
});

test('行视图模型：四态 / user 禁用 / mode 归一化 / 顺序保持', () => {
  const { view } = loadSettingsView();
  const rows = plain(
    view.buildProactivePersonaRows([
      { uid: 'char-0001', name: '小樱', kind: 'char', has_local_dialogue: true, mode: 'auto', effective: true },
      { uid: 'char-0002', name: '小凛', kind: 'char', has_local_dialogue: false, mode: 'auto', effective: false },
      { uid: 'char-0003', name: '手动开', kind: 'char', has_local_dialogue: false, mode: 'on', effective: true },
      { uid: 'char-0004', name: '手动关', kind: 'char', has_local_dialogue: true, mode: 'off', effective: false },
      { uid: 'user-0001', name: '我', kind: 'user', has_local_dialogue: true, mode: 'auto', effective: false },
      { uid: 'char-0005', name: '异常值', kind: 'char', has_local_dialogue: false, mode: 'garbage', effective: false },
    ])
  );

  assert.equal(rows.length, 6);
  // 顺序与读命令返回一致
  assert.deepEqual(
    rows.map((r) => r.uid),
    ['char-0001', 'char-0002', 'char-0003', 'char-0004', 'user-0001', 'char-0005']
  );
  assert.equal(rows[0].statusText, '自动开启（已对话过）');
  assert.equal(rows[0].selectDisabled, false);
  assert.equal(rows[1].statusText, '未解锁（对话一次后自动开启）');
  assert.equal(rows[2].statusText, '已开启（手动）');
  assert.equal(rows[3].statusText, '已关闭');
  assert.equal(rows[4].statusText, '用户人格不参与主动对话');
  assert.equal(rows[4].selectDisabled, true, 'user 行控件应禁用');
  assert.equal(rows[5].mode, 'auto', '未知 mode 应归一化为 auto');
  assert.equal(rows[5].statusText, '未解锁（对话一次后自动开启）');
});

test('行视图模型：缺失字段与非法输入防御', () => {
  const { view } = loadSettingsView();
  const rows = plain(view.buildProactivePersonaRows([
    { uid: 'char-0001' },
    { name: '缺 uid' },
    null,
  ]));

  assert.equal(rows.length, 1, '缺 uid 的行应被跳过');
  assert.equal(rows[0].name, 'char-0001', '缺 name 回退 uid');
  assert.equal(rows[0].mode, 'auto');
  assert.equal(rows[0].hasDialogue, false);

  assert.deepEqual(plain(view.buildProactivePersonaRows(null)), []);
  assert.deepEqual(plain(view.buildProactivePersonaRows('不是数组')), []);
});

// =========================================================
// 行内切换（处理器）
// =========================================================

test('切换成功：写命令调用正确并更新状态文案', async () => {
  const statusEl = { textContent: '' };
  const { view, calls } = loadSettingsView({
    elements: { 'settings-proactive-status-char-0001': statusEl },
  });

  const select = selectStub({
    value: 'on', uid: 'char-0001', prevMode: 'auto', hasDialogue: '1', kind: 'char',
  });
  await view.handleProactivePersonaChange(select);

  assert.deepEqual(calls.setCalls, [{ uid: 'char-0001', mode: 'on' }]);
  assert.equal(select.dataset.prevMode, 'on', '切换前值应更新');
  assert.equal(select.value, 'on');
  assert.equal(select.disabled, false, '保存完成后控件应恢复可用');
  assert.equal(statusEl.textContent, '已开启（手动）');
  assert.ok(calls.toasts.some((t) => t.type === 'success'), '应有成功提示');
  assert.ok(!calls.toasts.some((t) => t.type === 'info'), '已对话人格不应出现素材提示');
});

test('冷人格手动强开：素材提示但保存成功', async () => {
  const { view, calls } = loadSettingsView();

  const select = selectStub({
    value: 'on', uid: 'char-0009', prevMode: 'auto', hasDialogue: '0', kind: 'char',
  });
  await view.handleProactivePersonaChange(select);

  assert.deepEqual(calls.setCalls, [{ uid: 'char-0009', mode: 'on' }]);
  assert.ok(calls.toasts.some((t) => t.type === 'success'), '保存成功提示');
  const hint = calls.toasts.find((t) => t.type === 'info');
  assert.ok(hint, '冷人格强开应有素材提示');
  assert.equal(hint.title, '该人格尚未对话过');
});

test('切换失败：回滚控件与显示值并提示错误', async () => {
  const statusEl = { textContent: '未解锁（对话一次后自动开启）' };
  const { view, calls } = loadSettingsView({
    setFails: '数据库不可写',
    elements: { 'settings-proactive-status-char-0001': statusEl },
  });

  const select = selectStub({
    value: 'off', uid: 'char-0001', prevMode: 'auto', hasDialogue: '0', kind: 'char',
  });
  await view.handleProactivePersonaChange(select);

  assert.deepEqual(calls.setCalls, [{ uid: 'char-0001', mode: 'off' }]);
  assert.equal(select.value, 'auto', '失败应回滚到切换前值');
  assert.equal(select.dataset.prevMode, 'auto', '失败不得更新前值');
  assert.equal(select.disabled, false, '失败后控件应恢复可用');
  assert.equal(statusEl.textContent, '未解锁（对话一次后自动开启）', '状态文案不得显示为已生效');
  const errToast = calls.toasts.find((t) => t.type === 'error');
  assert.ok(errToast, '应有错误提示');
  assert.ok(String(errToast.detail).includes('数据库不可写'));
});

test('切换未变化：不调用写命令', async () => {
  const { view, calls } = loadSettingsView();

  const select = selectStub({
    value: 'auto', uid: 'char-0001', prevMode: 'auto', hasDialogue: '1', kind: 'char',
  });
  await view.handleProactivePersonaChange(select);

  assert.equal(calls.setCalls.length, 0, '值未变化不应调用写命令');
  assert.equal(calls.toasts.length, 0);
});

// =========================================================
// 列表加载（加载态 / 空态 / 错误态）
// =========================================================

test('名单加载成功：渲染行并绑定成功可查询', async () => {
  const container = {
    innerHTML: '',
    querySelectorAll() {
      return [];
    },
  };
  const { view, calls } = loadSettingsView({
    elements: { 'settings-proactive-roster': container },
    listResult: [
      { uid: 'user-0001', name: '我', kind: 'user', has_local_dialogue: true, mode: 'auto', effective: false },
      { uid: 'char-0001', name: '小樱', kind: 'char', has_local_dialogue: true, mode: 'auto', effective: true },
    ],
  });

  await view.loadProactivePersonas();

  assert.equal(calls.listCalls, 1);
  assert.ok(container.innerHTML.includes('user-0001'), '应渲染 user 行');
  assert.ok(container.innerHTML.includes('小樱'), '应渲染人格行');
  assert.ok(container.innerHTML.includes('disabled'), 'user 行控件应禁用');
  assert.ok(container.innerHTML.includes('用户人格不参与主动对话'));
});

test('空名单：显示空态文案', async () => {
  const container = { innerHTML: '', querySelectorAll: () => [] };
  const { view } = loadSettingsView({
    elements: { 'settings-proactive-roster': container },
    listResult: [],
  });

  await view.loadProactivePersonas();
  assert.ok(container.innerHTML.includes('暂无可管理的人格'));
});

test('名单加载失败：错误态提示且不回显旧值', async () => {
  const container = { innerHTML: '<div>旧值不应保留</div>', querySelectorAll: () => [] };
  const { view } = loadSettingsView({
    elements: { 'settings-proactive-roster': container },
    listFails: '存储不可用',
  });

  await view.loadProactivePersonas();
  assert.ok(container.innerHTML.includes('名单加载失败'), '应显示错误态');
  assert.ok(container.innerHTML.includes('存储不可用'));
  assert.ok(!container.innerHTML.includes('旧值不应保留'), '错误态不应回显旧值');
});
