/**
 * tests/settings-vision.test.js — 设置页「图片理解」回归（node --test）
 *
 * 覆盖:
 * - 单批上限输入组随能力声明勾选状态显隐
 * - 回显：能力声明与单批上限写入控件
 * - 收集与校验：非整数 / 负数拒绝；能力关闭时不校验
 * - 合并完整配置：纯函数不改入参；能力关闭时保留已配置上限
 *
 * 说明:
 * - settings.js 是浏览器 IIFE；本测试用 vm 注入 window/console/Router/document 桩加载，
 *   仅调用 `RamariaSettingsView` 暴露的纯函数与处理器，不触发真实渲染流程；
 * - 元素桩只实现被测逻辑用到的属性（checked / value / classList）。
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

/** 造一个带 classList 行为的元素桩 */
function classListElement(initialClasses) {
  const classes = new Set(initialClasses || []);
  return {
    classList: {
      add(name) { classes.add(name); },
      remove(name) { classes.delete(name); },
      toggle(name, force) {
        const on = force === undefined ? !classes.has(name) : !!force;
        if (on) classes.add(name);
        else classes.delete(name);
        return on;
      },
      contains(name) { return classes.has(name); },
    },
  };
}

/**
 * 在 vm 沙箱中加载 settings.js，返回视图单例与调用记录。
 *
 * 参数:
 * - `opts.elements`: document.getElementById 的 id → 元素桩映射（缺省 null）。
 */
function loadSettingsView(opts) {
  const options = opts || {};
  const source = fs.readFileSync(SETTINGS_PATH, 'utf8');
  const calls = { updateFull: [] };
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
    RamariaToast: {
      show() {},
    },
    RamariaApi: {
      config: {
        async updateFull(cfg) {
          calls.updateFull.push(cfg);
          return { fileOk: true, dbOk: true };
        },
      },
    },
    document: {
      getElementById(id) {
        const map = options.elements || {};
        return Object.prototype.hasOwnProperty.call(map, id) ? map[id] : null;
      },
    },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'settings.js' });

  const view = windowMock.RamariaSettingsView;
  if (!view || typeof view.getVisionFields !== 'function') {
    throw new Error('settings.js 未暴露图片理解面板接口');
  }
  return { view, calls };
}

// =========================================================
// 字段元数据
// =========================================================

test('图片理解字段元数据：2 键齐全且默认值正确', () => {
  const { view } = loadSettingsView();
  const fields = JSON.parse(JSON.stringify(view.getVisionFields()));
  const byPath = {};
  fields.forEach((f) => { byPath[f.path[0]] = f; });

  assert.deepEqual(Object.keys(byPath).sort(), ['batch_limit', 'model_supports_vision']);
  assert.equal(byPath.model_supports_vision.type, 'bool');
  assert.equal(byPath.model_supports_vision.def, false);
  assert.equal(byPath.batch_limit.type, 'number');
  assert.equal(byPath.batch_limit.def, 0);
  assert.equal(byPath.batch_limit.min, 0);

  // 高级设置不再重复登记同组键（避免两处编辑同一配置）
  const groups = JSON.parse(JSON.stringify(view.getAdvancedGroups()));
  assert.equal(groups.find((g) => g.key === 'vision'), undefined);
});

// =========================================================
// 表单显隐与回显
// =========================================================

test('单批上限输入组随能力声明显隐', () => {
  const enabledBox = { checked: false };
  const batchGroup = classListElement(['hidden']);
  const { view } = loadSettingsView({
    elements: {
      'settings-vision-enabled': enabledBox,
      'settings-vision-batch-group': batchGroup,
    },
  });

  view.syncVisionBatchVisibility();
  assert.ok(batchGroup.classList.contains('hidden'), '未勾选时上限输入组应隐藏');

  enabledBox.checked = true;
  view.syncVisionBatchVisibility();
  assert.ok(!batchGroup.classList.contains('hidden'), '勾选后上限输入组应显示');

  enabledBox.checked = false;
  view.syncVisionBatchVisibility();
  assert.ok(batchGroup.classList.contains('hidden'), '取消勾选后应重新隐藏');
});

test('回显：能力声明与单批上限写入控件并同步显隐', () => {
  const enabledBox = { checked: false };
  const batchInput = { value: '' };
  const batchGroup = classListElement(['hidden']);
  const { view } = loadSettingsView({
    elements: {
      'settings-vision-enabled': enabledBox,
      'settings-vision-batch-limit': batchInput,
      'settings-vision-batch-group': batchGroup,
    },
  });

  view.fillVisionForm({ vision: { model_supports_vision: true, batch_limit: 7 } });
  assert.equal(enabledBox.checked, true);
  assert.equal(batchInput.value, 7);
  assert.ok(!batchGroup.classList.contains('hidden'));

  view.fillVisionForm({ vision: { model_supports_vision: false, batch_limit: 3 } });
  assert.equal(enabledBox.checked, false);
  assert.equal(batchInput.value, 3, '上限值应照常回显（仅输入组隐藏）');
  assert.ok(batchGroup.classList.contains('hidden'));
});

// =========================================================
// 收集与校验
// =========================================================

test('收集：能力开启时校验上限（非整数 / 负数拒绝，留空按 0）', () => {
  const enabledBox = { checked: true };
  const batchInput = { value: '5' };
  const { view } = loadSettingsView({
    elements: {
      'settings-vision-enabled': enabledBox,
      'settings-vision-batch-limit': batchInput,
    },
  });

  assert.deepEqual(
    JSON.parse(JSON.stringify(view.collectVisionValues())),
    { enabled: true, batchLimit: 5 }
  );

  batchInput.value = '';
  assert.deepEqual(
    JSON.parse(JSON.stringify(view.collectVisionValues())),
    { enabled: true, batchLimit: 0 },
    '留空按 0 = 不限'
  );

  batchInput.value = 'abc';
  assert.ok(view.collectVisionValues().error, '非整数应拒绝');

  batchInput.value = '-1';
  assert.ok(view.collectVisionValues().error, '负数应拒绝');
});

test('收集：能力关闭时不校验上限输入', () => {
  const enabledBox = { checked: false };
  const batchInput = { value: 'abc' };
  const { view } = loadSettingsView({
    elements: {
      'settings-vision-enabled': enabledBox,
      'settings-vision-batch-limit': batchInput,
    },
  });

  const collected = JSON.parse(JSON.stringify(view.collectVisionValues()));
  assert.equal(collected.enabled, false);
  assert.equal(collected.error, undefined, '能力关闭时输入框不参与校验');
});

// =========================================================
// 合并完整配置（纯函数）
// =========================================================

test('合并配置：不改入参；开启时写入上限、关闭时保留已配置上限', () => {
  const { view } = loadSettingsView();
  const base = { vision: { model_supports_vision: false, batch_limit: 9 }, backend: { provider: 'deepseek' } };

  const enabled = JSON.parse(JSON.stringify(
    view.applyVisionFormToConfig(base, { enabled: true, batchLimit: 5 })
  ));
  assert.deepEqual(enabled.vision, { model_supports_vision: true, batch_limit: 5 });
  assert.deepEqual(base.vision, { model_supports_vision: false, batch_limit: 9 }, '入参不应被修改');

  const disabled = JSON.parse(JSON.stringify(
    view.applyVisionFormToConfig(base, { enabled: false, batchLimit: 5 })
  ));
  assert.deepEqual(
    disabled.vision,
    { model_supports_vision: false, batch_limit: 9 },
    '关闭声明时保留已配置的上限（重新开启后沿用）'
  );

  // 缺失 vision 段时补建
  const created = JSON.parse(JSON.stringify(
    view.applyVisionFormToConfig({ backend: {} }, { enabled: true, batchLimit: 0 })
  ));
  assert.deepEqual(created.vision, { model_supports_vision: true, batch_limit: 0 });
});
