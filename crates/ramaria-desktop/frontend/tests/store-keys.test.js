/**
 * tests/store-keys.test.js — Store 状态字段注册一致性回归（node --test）
 *
 * 目的:
 * - 锁定 `debugEnabled` 等字段必须在 Store 注册：未注册时 `get`/`set` 会打印
 *   "[Store] 未知状态字段: xxx" 告警并**静默丢弃写入**（控制台噪音 + 状态不生效）；
 * - 扫描全部前端 JS 中字面量形式的 `RamariaStore.get/set('<key>')` 调用，逐一断言
 *   字段已注册——后续新增字段忘记登记时本测试直接失败。
 *
 * 说明:
 * - store.js 是浏览器 IIFE，本测试用 vm 注入 window/console 桩加载，只调用其公开 API
 *   （`get` 对未知字段告警，可作为"是否注册"的黑盒探针）；
 * - 仅约束 `get`/`set`（两者都会对未知字段告警）；`subscribe` 允许批量事件等
 *   伪事件名（如 `ready`），不做约束。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** Store 源码路径（frontend/js/store.js） */
const STORE_PATH = path.resolve(__dirname, '..', 'js', 'store.js');

/** 前端 js 根目录（扫描使用中的 Store 字段） */
const JS_DIR = path.resolve(__dirname, '..', 'js');

/**
 * 在 vm 沙箱中加载 store.js。
 *
 * 返回:
 * - `store`: 全局单例 RamariaStore；
 * - `warnings`: 捕获到的 console.warn 文本数组（`未知状态字段` 探针）。
 */
function loadStore() {
  const source = fs.readFileSync(STORE_PATH, 'utf8');
  const warnings = [];
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: {
      log() {},
      warn(msg) {
        warnings.push(String(msg));
      },
      error() {},
    },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'store.js' });

  const store = windowMock.RamariaStore;
  if (!store) throw new Error('store.js 未暴露 RamariaStore');
  return { store, warnings };
}

/** 递归收集目录下全部 .js 文件 */
function walkJsFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkJsFiles(full));
    else if (entry.name.endsWith('.js')) out.push(full);
  }
  return out;
}

/** 扫描源码中字面量形式的 `RamariaStore.get/set('key', ...)` 字段名 */
function collectUsedStateKeys() {
  const keys = new Set();
  const pattern = /RamariaStore\.(?:get|set)\(\s*['"]([^'"]+)['"]/g;

  for (const file of walkJsFiles(JS_DIR)) {
    const source = fs.readFileSync(file, 'utf8');
    let match;
    while ((match = pattern.exec(source)) !== null) {
      keys.add(match[1]);
    }
  }
  return keys;
}

test('Store: debugEnabled 已注册（get/set 不触发未知字段告警）', () => {
  const { store, warnings } = loadStore();

  assert.equal(store.get('debugEnabled'), false, '初始值应为 false');
  store.set('debugEnabled', true, true);
  assert.equal(store.get('debugEnabled'), true, '写入后应可读回');
  assert.deepEqual(warnings, [], `不应出现未知字段告警: ${warnings.join(' | ')}`);
});

test('Store: reset() 后 debugEnabled 仍在注册表内', () => {
  const { store, warnings } = loadStore();

  store.set('debugEnabled', true, true);
  store.reset();
  // reset 会重建 _state：字段若漏登记，这里会重新出现告警且写入被丢弃
  store.set('debugEnabled', true, true);
  assert.equal(store.get('debugEnabled'), true);
  assert.deepEqual(warnings, [], `reset 后不应出现未知字段告警: ${warnings.join(' | ')}`);
});

test('Store: 全量扫描——所有前端使用的状态字段均已注册', () => {
  const { store, warnings } = loadStore();
  const usedKeys = collectUsedStateKeys();

  assert.ok(usedKeys.size > 0, '未扫描到任何 RamariaStore.get/set 调用（扫描逻辑失效）');

  for (const key of usedKeys) {
    store.get(key);  // 未知字段会在此写入 console.warn
  }

  assert.deepEqual(
    warnings,
    [],
    `以下字段未在 store.js 的 _state 中注册: ${warnings.join(' | ')}`
  );
});
