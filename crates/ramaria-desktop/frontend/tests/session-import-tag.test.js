/**
 * tests/session-import-tag.test.js — 导入会话识别回归（node --test）
 *
 * 目的（v2.8 M4「判定显式化」验收）:
 * - 导入会话按归属 persona 的 `source`（显式载体）识别：source = "qq" → 导入会话；
 * - `user-{uin}` 归属的导入会话（导出者本人）被正确识别；
 * - 非导入来源（人格文件 / 系统用户人格）即使是 char- 前缀也不误标；
 * - 缺数据防御：persona 未加载 / 无归属 / 空输入 → 非导入。
 *
 * 说明:
 * - session-drawer.js 是浏览器 IIFE 组件；本测试用 vm 注入 window / console /
 *   RamariaStore 桩加载，只调用纯函数 `isImportedSession(session)`，不触碰 DOM 与 Tauri。
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
 * 参数:
 * - `options.personas`: RamariaStore.get('personas') 返回值（缺省为空数组）。
 */
function loadDrawer(options) {
  const opts = options || {};
  const source = fs.readFileSync(DRAWER_PATH, 'utf8');

  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    RamariaStore: { get: () => opts.personas || [] },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'session-drawer.js' });

  const drawer = windowMock.RamariaSessionDrawer;
  if (!drawer || typeof drawer.isImportedSession !== 'function') {
    throw new Error('session-drawer.js 未暴露 isImportedSession()');
  }
  return drawer;
}

const imported = loadDrawer({
  personas: [
    { uid: 'user-10001', name: '小明', source: 'qq' },
    { uid: 'char-90002', name: '小红', source: 'qq' },
    { uid: 'char-file-1', name: '文件人格', source: 'file' },
    { uid: 'user-0001', name: '用户', source: 'system' },
    { uid: 'anim-0001', name: '原创角色' },
  ],
});

test('导入会话识别：user-{uin} 归属的导入会话（导出者本人）被识别', () => {
  assert.equal(
    imported.isImportedSession({ persona_uid: 'user-10001' }),
    true,
    '导出者 persona（source=qq）归属的会话应为导入会话'
  );
});

test('导入会话识别：char- 归属的导入会话被识别', () => {
  assert.equal(imported.isImportedSession({ persona_uid: 'char-90002' }), true);
});

test('导入会话识别：char- 前缀但非导入来源（人格文件）不误标', () => {
  assert.equal(
    imported.isImportedSession({ persona_uid: 'char-file-1' }),
    false,
    'source=file 的 char- 前缀人格不属于导入会话'
  );
});

test('导入会话识别：系统用户人格不标记', () => {
  assert.equal(imported.isImportedSession({ persona_uid: 'user-0001' }), false);
});

test('导入会话识别：source 缺失按非导入处理', () => {
  assert.equal(imported.isImportedSession({ persona_uid: 'anim-0001' }), false);
});

test('缺数据防御：persona 未加载 / 无归属 / 空输入 → 非导入', () => {
  assert.equal(imported.isImportedSession({ persona_uid: 'char-not-loaded' }), false);
  assert.equal(imported.isImportedSession({}), false);
  assert.equal(imported.isImportedSession({ persona_uid: null }), false);
  assert.equal(imported.isImportedSession(null), false);
  assert.equal(imported.isImportedSession(undefined), false);
});

test('缺数据防御：personas 列表为空时不抛错', () => {
  const empty = loadDrawer({});
  assert.equal(empty.isImportedSession({ persona_uid: 'user-10001' }), false);
});
