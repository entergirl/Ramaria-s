/**
 * tests/session-unread.test.js — 会话未读徽标回归（node --test）
 *
 * 覆盖:
 * - 未读计数 > 0 时返回计数徽标（class 统一，title 含完整计数文案）；
 * - 超过 99 显示 "99+"；
 * - 0 / 缺字段（存量数据）/ 非数字 / 负值 / 空输入返回 null（不显示徽标）。
 *
 * 说明:
 * - session-drawer.js 是浏览器 IIFE 组件；本测试用 vm 注入 window/console 桩加载，
 *   只调用纯函数 `unreadBadge(session)`，不触碰 DOM、Store 与 Tauri。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** 会话抽屉组件源码（frontend/js/components/session-drawer.js） */
const DRAWER_PATH = path.resolve(__dirname, '..', 'js', 'components', 'session-drawer.js');

/**
 * 在 vm 沙箱中加载 session-drawer.js，返回其全局单例。
 *
 * 桩说明:
 * - 组件在函数体内按需使用 DOM / RamariaStore / RamariaFormat / RamariaApi，
 *   加载期只执行 IIFE 定义与 `Object.defineProperty(window, ...)`，
 *   故无需注入这些依赖（测试只调用纯函数 `unreadBadge`）。
 */
function loadDrawer() {
  const source = fs.readFileSync(DRAWER_PATH, 'utf8');

  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'session-drawer.js' });

  const drawer = windowMock.RamariaSessionDrawer;
  if (!drawer || typeof drawer.unreadBadge !== 'function') {
    throw new Error('session-drawer.js 未暴露 unreadBadge()');
  }
  return drawer;
}

const drawer = loadDrawer();

/**
 * 调用 unreadBadge 并把沙箱返回值归一为宿主 realm 对象（避免原型不同导致的断言误报）。
 */
function badge(session) {
  const result = drawer.unreadBadge(session);
  return result == null ? result : JSON.parse(JSON.stringify(result));
}

test('未读计数大于 0：返回计数徽标', () => {
  const result = badge({ unread: 3 });
  assert.ok(result, '应有未读徽标');
  assert.equal(result.text, '3');
  assert.equal(result.className, 'session-drawer-item-unread');
  assert.ok(result.title.includes('3'), `title 应包含计数: ${result.title}`);
});

test('未读计数超过 99：显示 99+（99 仍显示原值）', () => {
  assert.equal(badge({ unread: 120 }).text, '99+');
  assert.equal(badge({ unread: 99 }).text, '99');
  assert.equal(badge({ unread: 100 }).text, '99+');
});

test('无未读 / 缺字段 / 非法值：不显示徽标', () => {
  assert.equal(badge({ unread: 0 }), null);
  assert.equal(badge({}), null, '缺 unread 字段（存量数据）不应显示');
  assert.equal(badge({ unread: '3' }), null, '非数字不应显示');
  assert.equal(badge({ unread: -1 }), null, '负值不应显示');
  assert.equal(badge(null), null);
  assert.equal(badge(undefined), null);
});
