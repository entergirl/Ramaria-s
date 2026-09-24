/**
 * tests/mcp-snippets.test.js — MCP 客户端配置片段生成回归（node --test）
 *
 * 目的（v2.1 M5「配置片段与提示文案」验收）:
 * - 锁定「复制即用」：三种客户端（Claude Desktop / Cursor / OpenClaw）片段均为
 *   合法 JSON、命令与参数完整、库路径经 JSON 转义后可完整还原（Windows 反斜杠不丢失）；
 * - 锁定参数顺序约定：`--db` 为 CLI 全局参数，置于 `mcp serve` 子命令之前；
 * - 锁定空态防御：缺少命令或库路径时返回空数组（面板显示空态而非残缺片段）。
 *
 * 说明:
 * - settings.js 是浏览器 IIFE 视图，本测试用 vm 注入最小桩加载，
 *   只调用纯函数 `buildMcpClientSnippets(info)`，不触碰 DOM 与 Tauri。
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

/**
 * 在 vm 沙箱中加载 settings.js，返回其全局单例。
 *
 * 桩说明（与 settings-defaults.test.js 同口径）:
 * - `RamariaRouter`：init() 只注册钩子；getCurrentView 返回非 settings，
 *   避免自动初始化分支注册定时器导致测试进程挂起；
 * - 不注入 `document`：本测试只调用纯函数，不触碰 DOM。
 */
function loadSettingsView() {
  const source = fs.readFileSync(SETTINGS_PATH, 'utf8');

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
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'settings.js' });

  const view = windowMock.RamariaSettingsView;
  if (!view || typeof view.buildMcpClientSnippets !== 'function') {
    throw new Error('settings.js 未暴露 buildMcpClientSnippets()');
  }
  return view;
}

const settingsView = loadSettingsView();

/** 样例信息：含空格与反斜杠的 Windows 风格命令与库路径（覆盖 JSON 转义场景） */
const SAMPLE = {
  command: 'C:\\Program Files\\Ramaria\\ramaria.exe',
  dbPath: 'C:\\Users\\tester\\AppData\\Roaming\\Ramaria\\data\\assistant.db',
};

// 经 JSON 往返归一为宿主 realm 对象（vm 沙箱返回值原型不同，避免 deepStrictEqual 误报）
const snippets = JSON.parse(JSON.stringify(settingsView.buildMcpClientSnippets(SAMPLE)));

/** 提取片段中的 stdio 节点（Claude Desktop / Cursor 为 mcpServers；OpenClaw 为 mcp.servers） */
function stdioOf(snippet) {
  const parsed = JSON.parse(snippet.content);
  return snippet.key === 'openclaw'
    ? parsed.mcp && parsed.mcp.servers && parsed.mcp.servers.ramaria
    : parsed.mcpServers && parsed.mcpServers.ramaria;
}

test('三种客户端片段齐全且带配置文件位置提示', () => {
  assert.deepEqual(
    snippets.map((s) => s.key),
    ['claude-desktop', 'cursor', 'openclaw']
  );

  for (const s of snippets) {
    assert.ok(s.title, `${s.key}: 缺少标题`);
    assert.ok(s.fileHint, `${s.key}: 缺少配置文件位置提示`);
    assert.ok(
      typeof s.content === 'string' && s.content.length > 0,
      `${s.key}: 缺少片段内容`
    );
  }
});

test('片段为合法 JSON 且命令/参数完整（--db 置于子命令前）', () => {
  for (const s of snippets) {
    const stdio = stdioOf(s);
    assert.ok(stdio, `${s.key}: 缺少 ramaria 节点`);
    assert.equal(stdio.command, SAMPLE.command, `${s.key}: command 不一致`);
    assert.deepEqual(
      stdio.args,
      ['--db', SAMPLE.dbPath, 'mcp', 'serve'],
      `${s.key}: args 不一致（--db 为全局参数，应位于 mcp serve 之前）`
    );
  }
});

test('Windows 路径经 JSON 转义后可完整还原', () => {
  for (const s of snippets) {
    const stdio = stdioOf(s);
    assert.equal(stdio.args[1], SAMPLE.dbPath, `${s.key}: 库路径还原不一致`);
    assert.ok(
      s.content.includes('\\\\'),
      `${s.key}: JSON 文本应包含转义后的反斜杠（否则路径无法粘贴使用）`
    );
  }
});

test('OpenClaw 片段附带命令行添加方式（含命令与库路径）', () => {
  const openclaw = snippets.find((s) => s.key === 'openclaw');
  assert.ok(openclaw.note && openclaw.note.includes('openclaw mcp add'), '缺少 CLI 提示');
  assert.ok(openclaw.note.includes(SAMPLE.command), 'CLI 提示应包含命令');
  assert.ok(openclaw.note.includes(SAMPLE.dbPath), 'CLI 提示应包含库路径');
});

test('参数缺失时返回空数组（面板显示空态而非残缺片段）', () => {
  for (const input of [{}, { command: 'ramaria' }, { dbPath: SAMPLE.dbPath }, null, undefined]) {
    assert.deepEqual(
      JSON.parse(JSON.stringify(settingsView.buildMcpClientSnippets(input))),
      [],
      `输入 ${JSON.stringify(input)} 应返回空数组`
    );
  }
});
