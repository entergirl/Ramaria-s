/**
 * tests/mcp-snippets.test.js — MCP 挂载配置片段生成回归（node --test）
 *
 * 目的（v2.1 M5「配置片段与提示文案」验收）:
 * - 通用片段（JSON）：合法 JSON、命令与参数完整、库路径经 JSON 转义可完整还原
 *   （Windows 反斜杠不丢失）、`--db` 置于 `mcp serve` 子命令之前；
 * - dsh 片段（YAML）：Cordis 插件字段完整（`@deepseek-ai/dsh-mcp-client` /
 *   `transport: stdio` / `serverName`），路径用单引号包裹（反斜杠不转义）、
 *   含"勿覆盖已有内容"提示；内部单引号按 YAML 规则转义为两个；
 * - 空态防御：缺少命令或库路径时返回空数组（面板显示空态而非残缺片段）。
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

/** 样例信息：含空格与反斜杠的 Windows 风格命令与库路径（覆盖转义场景） */
const SAMPLE = {
  command: 'C:\\Program Files\\Ramaria\\ramaria.exe',
  dbPath: 'C:\\Users\\tester\\AppData\\Roaming\\Ramaria\\data\\assistant.db',
};

// 经 JSON 往返归一为宿主 realm 对象（vm 沙箱返回值原型不同，避免 deepStrictEqual 误报）
const snippets = JSON.parse(JSON.stringify(settingsView.buildMcpClientSnippets(SAMPLE)));
const generic = snippets.find((s) => s.key === 'generic');
const dsh = snippets.find((s) => s.key === 'dsh');

test('两段片段齐全：通用配置（JSON）+ DeepSeek Harness（YAML）', () => {
  assert.deepEqual(
    snippets.map((s) => s.key),
    ['generic', 'dsh']
  );
  assert.equal(generic.format, 'json');
  assert.equal(dsh.format, 'yaml');

  for (const s of snippets) {
    assert.ok(s.title, `${s.key}: 缺少标题`);
    assert.ok(s.fileHint, `${s.key}: 缺少放置位置提示`);
    assert.ok(s.note, `${s.key}: 缺少补充说明`);
    assert.ok(typeof s.content === 'string' && s.content.length > 0, `${s.key}: 缺少片段内容`);
  }
});

test('通用 JSON：结构合法且命令/参数正确（--db 置于子命令前）', () => {
  const parsed = JSON.parse(generic.content);
  const stdio = parsed.mcpServers && parsed.mcpServers.ramaria;
  assert.ok(stdio, '缺少 mcpServers.ramaria 节点');
  assert.equal(stdio.command, SAMPLE.command, 'command 不一致');
  assert.deepEqual(
    stdio.args,
    ['--db', SAMPLE.dbPath, 'mcp', 'serve'],
    'args 不一致（--db 为全局参数，应位于 mcp serve 之前）'
  );
});

test('通用 JSON：Windows 路径经转义后可完整还原', () => {
  assert.ok(
    generic.content.includes('\\\\'),
    'JSON 文本应包含转义后的反斜杠（否则路径无法粘贴使用）'
  );
  const parsed = JSON.parse(generic.content);
  assert.equal(parsed.mcpServers.ramaria.args[1], SAMPLE.dbPath, '库路径还原不一致');
});

test('dsh YAML：插件字段完整且路径用单引号包裹', () => {
  const yaml = dsh.content;
  assert.ok(yaml.includes("name: '@deepseek-ai/dsh-mcp-client'"), '缺少 MCP 客户端插件名');
  assert.ok(yaml.includes('serverName: ramaria'), '缺少 serverName');
  assert.ok(yaml.includes('transport: stdio'), '缺少 stdio 传输声明');
  assert.ok(yaml.includes('command: ' + "'" + SAMPLE.command + "'"), 'command 应为单引号包裹');
  assert.ok(yaml.includes("'" + SAMPLE.dbPath + "'"), '库路径应为单引号包裹');
  assert.ok(yaml.includes('insert:'), '应为可合并的 insert patch 片段');
});

test('dsh YAML：不做 JSON 式转义，且含勿覆盖提示', () => {
  const yaml = dsh.content;
  assert.ok(
    !yaml.includes('\\\\'),
    'YAML 单引号内反斜杠不应被转义为 JSON 式双反斜杠'
  );
  assert.ok(dsh.fileHint.includes('不要覆盖'), '位置提示应说明勿覆盖已有 patch 内容');
  assert.ok(dsh.note.includes('mcp__ramaria__'), '补充说明应给出工具名前缀');
});

test('dsh YAML：字符串内含单引号时按 YAML 规则转义为两个', () => {
  const view = JSON.parse(
    JSON.stringify(
      settingsView.buildMcpClientSnippets({
        command: "C:\\It's\\ramaria.exe",
        dbPath: SAMPLE.dbPath,
      })
    )
  );
  const yaml = view.find((s) => s.key === 'dsh').content;
  assert.ok(
    yaml.includes("C:\\It''s\\ramaria.exe"),
    "单引号应转义为两个（YAML 单引号字符串规则）"
  );
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
