/**
 * tests/session-source-tag.test.js — 会话来源标注回归（node --test）
 *
 * 目的（v2.1 M6「桌面会话列表标注来源」验收）:
 * - `channel = mcp` 的会话返回「来源: MCP」标签，外部对话标识进入 `title`；
 * - `channel = local` / 缺少通道字段（存量数据）不标注；
 * - 未知非本地通道回退显示通道名（后续 telegram / qq 等通道沿用同一入口）；
 * - 空值防御：null / undefined / 缺字段输入返回 null。
 *
 * 说明:
 * - session-drawer.js 是浏览器 IIFE 组件；本测试用 vm 注入 window/console 桩加载，
 *   只调用纯函数 `sourceTag(session)`，不触碰 DOM、Store 与 Tauri。
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
 *   故无需注入这些依赖（测试只调用纯函数 `sourceTag`）。
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
  if (!drawer || typeof drawer.sourceTag !== 'function') {
    throw new Error('session-drawer.js 未暴露 sourceTag()');
  }
  return drawer;
}

const drawer = loadDrawer();

/**
 * 调用 sourceTag 并把沙箱返回值归一为宿主 realm 对象（避免原型不同导致的断言误报）。
 */
function tag(session) {
  const result = drawer.sourceTag(session);
  return result == null ? result : JSON.parse(JSON.stringify(result));
}

test('MCP 会话：返回来源标签并携带外部标识', () => {
  const result = tag({ channel: 'mcp', external_ref: 'client-A' });
  assert.ok(result, 'MCP 会话应有来源标签');
  assert.equal(result.text, '来源: MCP');
  assert.ok(
    result.className.includes('session-drawer-item-tag--source'),
    `应使用来源标签样式: ${result.className}`
  );
  assert.ok(result.title.includes('client-A'), `title 应包含外部标识: ${result.title}`);
});

test('MCP 会话（无外部标识）：title 回退为通道说明', () => {
  const result = tag({ channel: 'mcp' });
  assert.equal(result.text, '来源: MCP');
  assert.ok(result.title.includes('MCP'), `title 应说明通道: ${result.title}`);
});

test('本地会话与存量数据：不标注', () => {
  assert.equal(tag({ channel: 'local' }), null, '本地会话不应标注');
  assert.equal(tag({}), null, '缺少 channel（存量会话）不应标注');
  assert.equal(tag(null), null);
  assert.equal(tag(undefined), null);
});

test('未知通道：回退显示通道名（后续社交通道沿用同一入口）', () => {
  const result = tag({ channel: 'telegram' });
  assert.ok(result, '未知非本地通道仍应标注');
  assert.equal(result.text, '来源: telegram');
});
