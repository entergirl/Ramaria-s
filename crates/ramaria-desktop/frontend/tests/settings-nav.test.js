/**
 * tests/settings-nav.test.js — 设置页目录导航与迁移完整性回归（node --test）
 *
 * 覆盖:
 * - 基础目录表（5 组 12 项）：key 集合、分组归属、标题与现小标题一致
 * - 高级目录表（6 组 18 项）：key 集合与高级分组元数据完全一致、分组归属
 * - 基础↔高级映射表：两侧 key 均在目录表中、反向表同源可达
 * - 深链解析：合法 / 非法输入、未知 key 降级约定
 * - index.html：settings 模块 script 序列与模块加载清单一致（旧单文件不再引用）
 * - 公开 API：键集合完整、逐一为函数
 * - 目录项挂载面：12 个基础 section 均暴露 render（有回显的暴露 fill）
 *
 * 说明:
 * - 经 tests/helpers/load-settings.js 在 vm 沙箱中按序加载设置页全部模块；
 * - vm 沙箱返回值原型与宿主 realm 不同，断言前经 JSON 往返归一。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { loadSettingsView, SETTINGS_MODULES } = require('./helpers/load-settings.js');

/** 前端 index.html（script 加载序列核对） */
const INDEX_HTML_PATH = path.resolve(__dirname, '..', 'index.html');

/** JSON 往返归一（vm realm 对象 → 宿主 realm） */
function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

const { view, windowMock } = loadSettingsView();
const NS = windowMock.RamariaSettingsNS;

/** 公开 API 键集合（与原单文件设置页返回块逐一对应） */
const EXPECTED_API = [
  'init',
  'getAdvancedGroups',
  'getMcpFields',
  'getVisionFields',
  'fillVisionForm',
  'syncVisionBatchVisibility',
  'collectVisionValues',
  'applyVisionFormToConfig',
  'buildMcpClientSnippets',
  'buildProactivePersonaRows',
  'proactivePersonaStatusText',
  'handleProactivePersonaChange',
  'loadProactivePersonas',
  'getChannelFields',
  'buildChannelBindingRows',
  'channelStateView',
  'channelCredentialKey',
  'applyChannelFormToConfig',
  'saveChannelCredential',
  'handleSaveChannelCredential',
  'destroy',
];

/** 11 个基础目录项挂载面（fill 表示该目录项有同步回显入口） */
const BASIC_FACES = [
  { name: 'backend', fill: true },
  { name: 'embedding', fill: true },
  { name: 'session', fill: true },
  { name: 'proactive', fill: true },
  { name: 'channel', fill: false },
  { name: 'privacy', fill: false },
  { name: 'dataDir', fill: true },
  { name: 'mcp', fill: true },
  { name: 'data', fill: false },
  { name: 'diagnostics', fill: false },
  { name: 'about', fill: false },
];

// =========================================================
// 目录表完整性
// =========================================================

test('基础目录：5 组 12 项，key 集合与分组归属正确', () => {
  const nav = plain(NS.NAV_BASIC);
  assert.equal(nav.length, 5, '基础目录应为 5 个一级分组');
  assert.deepEqual(
    nav.map((g) => g.title),
    ['模型与检索', '对话与主动', '社交与接入', '隐私与数据', '系统']
  );

  const allKeys = nav.flatMap((g) => g.items.map((i) => i.key)).sort();
  assert.equal(allKeys.length, 11, '基础目录应为 11 项');
  assert.deepEqual(allKeys, [
    'about', 'backend', 'channel', 'data', 'data_dir', 'diagnostics',
    'embedding', 'mcp', 'privacy', 'proactive', 'session',
  ].sort());

  // 分组归属（IA 稿 §3.2）
  const byTitle = {};
  nav.forEach((g) => { byTitle[g.title] = g.items.map((i) => i.key); });
  assert.deepEqual(byTitle['模型与检索'], ['backend', 'embedding']);
  assert.deepEqual(byTitle['对话与主动'], ['session', 'proactive']);
  assert.deepEqual(byTitle['社交与接入'], ['channel', 'mcp']);
  assert.deepEqual(byTitle['隐私与数据'], ['privacy', 'data_dir', 'data']);
  assert.deepEqual(byTitle['系统'], ['diagnostics', 'about']);
});

test('基础目录：目录项标题与现小标题一致', () => {
  const titles = {};
  plain(NS.NAV_BASIC).forEach((g) => {
    g.items.forEach((i) => { titles[i.key] = i.title; });
  });

  assert.equal(titles.backend, '🔌 后端配置');
  assert.equal(titles.embedding, '🧲 嵌入模型');
  assert.equal(titles.session, '💬 会话');
  assert.equal(titles.proactive, '🗨️ 主动对话');
  assert.equal(titles.channel, '💬 社交平台接入');
  assert.equal(titles.privacy, '🔒 隐私设置');
  assert.equal(titles.data_dir, '📂 数据目录');
  assert.equal(titles.mcp, '📡 MCP 接入');
  assert.equal(titles.data, '💾 数据管理');
  assert.equal(titles.diagnostics, '🔧 诊断与更新');
  assert.equal(titles.about, 'ℹ️ 关于');
});

test('高级目录：6 组 18 项，key 集合与高级分组元数据完全一致', () => {
  const nav = plain(NS.NAV_ADVANCED);
  assert.equal(nav.length, 6, '高级目录应为 6 个一级分组');
  assert.deepEqual(
    nav.map((g) => g.title),
    ['检索与索引', '记忆管线', '注入与装配', '人格与知识', '主动与通道', '系统']
  );

  const allKeys = nav.flatMap((g) => g.keys);
  assert.equal(allKeys.length, 18, '高级目录应为 18 项');
  assert.equal(new Set(allKeys).size, 18, '高级目录 key 不得重复');

  const groups = plain(view.getAdvancedGroups());
  assert.deepEqual(
    allKeys.slice().sort(),
    groups.map((g) => g.key).sort(),
    '目录 key 集合应与高级分组元数据完全一致（无漏无重）'
  );

  // 分组归属（IA 稿 §3.3）
  const byTitle = {};
  nav.forEach((g) => { byTitle[g.title] = g.keys; });
  assert.deepEqual(byTitle['检索与索引'], ['retrieval', 'index']);
  assert.deepEqual(byTitle['记忆管线'], ['decay', 'thresholds', 'event_extraction', 'l1-progressive']);
  assert.deepEqual(byTitle['注入与装配'], ['utt', 'examples', 'bridge', 'injection_budget', 'layer_dedup']);
  assert.deepEqual(byTitle['人格与知识'], ['inference', 'knowledge', 'style', 'inference-upgrade']);
  assert.deepEqual(byTitle['主动与通道'], ['proactive', 'channel']);
  assert.deepEqual(byTitle['系统'], ['logging']);
});

// =========================================================
// 基础↔高级映射
// =========================================================

test('基础↔高级映射：两侧 key 均在目录表中且反向同源', () => {
  const basicToAdvanced = plain(NS.CROSSLINKS.basicToAdvanced);
  const advancedToBasic = plain(NS.CROSSLINKS.advancedToBasic);
  const basicKeys = new Set(plain(NS.NAV_BASIC).flatMap((g) => g.items.map((i) => i.key)));
  const advKeys = new Set(plain(NS.NAV_ADVANCED).flatMap((g) => g.keys));

  assert.ok(Object.keys(basicToAdvanced).length > 0, '映射表不应为空');
  for (const [basicKey, targets] of Object.entries(basicToAdvanced)) {
    assert.ok(basicKeys.has(basicKey), `映射源 ${basicKey} 应在基础目录中`);
    for (const advKey of targets) {
      assert.ok(advKeys.has(advKey), `映射目标 ${advKey} 应在高级目录中`);
      assert.ok(
        advancedToBasic[advKey].includes(basicKey),
        `${advKey} 的反向映射应包含 ${basicKey}（两侧可达）`
      );
    }
  }
});

test('基础↔高级映射：D4 基线逐项核对', () => {
  const basicToAdvanced = plain(NS.CROSSLINKS.basicToAdvanced);
  assert.deepEqual(basicToAdvanced.backend, ['retrieval', 'index']);
  assert.deepEqual(basicToAdvanced.embedding, ['retrieval']);
  assert.deepEqual(basicToAdvanced.session, ['decay', 'thresholds', 'l1-progressive']);
  assert.deepEqual(basicToAdvanced.proactive, ['proactive']);
  assert.deepEqual(basicToAdvanced.channel, ['channel']);
});

// =========================================================
// 深链解析
// =========================================================

test('深链解析：合法输入返回 {tab,key}', () => {
  assert.deepEqual(
    plain(NS.parseSettingsHash('#settings/basic/backend')),
    { tab: 'basic', key: 'backend' }
  );
  assert.deepEqual(
    plain(NS.parseSettingsHash('#settings/advanced/retrieval')),
    { tab: 'advanced', key: 'retrieval' }
  );
  assert.deepEqual(
    plain(NS.parseSettingsHash('#settings/advanced/l1-progressive')),
    { tab: 'advanced', key: 'l1-progressive' }
  );
});

test('深链解析：非法输入返回 null', () => {
  const bads = [
    '',
    '#settings',
    '#settings/basic',
    '#settings/basic/backend/extra',
    '#settings/other/backend',
    '#chat',
    'settings/basic/backend',
    null,
    undefined,
    123,
  ];
  for (const bad of bads) {
    assert.equal(NS.parseSettingsHash(bad), null, `输入 ${JSON.stringify(bad)} 应返回 null`);
  }
});

test('目录项解析：合法 key 命中、未知 key 返回 null（降级约定）', () => {
  const basic = plain(NS.resolveNavEntry('basic', 'backend'));
  assert.equal(basic.tab, 'basic');
  assert.equal(basic.key, 'backend');
  assert.equal(basic.group, '模型与检索');
  assert.ok(basic.title, '基础项应带标题');

  const adv = plain(NS.resolveNavEntry('advanced', 'retrieval'));
  assert.equal(adv.tab, 'advanced');
  assert.equal(adv.key, 'retrieval');
  assert.equal(adv.group, '检索与索引');

  assert.equal(NS.resolveNavEntry('basic', 'nope'), null);
  assert.equal(NS.resolveNavEntry('advanced', 'nope'), null);
  assert.equal(NS.resolveNavEntry('other', 'backend'), null);
});

// =========================================================
// index.html 与公开 API
// =========================================================

test('index.html：settings 模块 script 序列与加载清单一致', () => {
  const html = fs.readFileSync(INDEX_HTML_PATH, 'utf8');
  const pattern = /<script src="js\/views\/settings\/([^"]+)"><\/script>/g;
  const found = [];
  let m;
  while ((m = pattern.exec(html)) !== null) found.push(m[1]);

  assert.deepEqual(found, SETTINGS_MODULES, 'index.html 的 settings 模块序列应与加载清单一致');
  assert.equal(found.length, 15, 'settings 模块应恰为 15 个');

  // 旧单文件不再被引用（新目录为唯一实现）
  assert.ok(!html.includes('js/views/settings.js'), '旧单文件 js/views/settings.js 不应再被引用');
});

test('公开 API：键集合与预期清单一致且全部为函数', () => {
  assert.deepEqual(Object.keys(view).sort(), EXPECTED_API.slice().sort());
  for (const key of EXPECTED_API) {
    assert.equal(typeof view[key], 'function', `${key} 应为函数`);
  }
});

test('目录项挂载面：11 个基础 section 均暴露 render（有回显的暴露 fill）', () => {
  for (const face of BASIC_FACES) {
    const part = NS.parts[face.name];
    assert.ok(part, `NS.parts.${face.name} 应存在`);
    assert.equal(typeof part.render, 'function', `${face.name}.render 应为函数`);
    if (face.fill) {
      assert.equal(typeof part.fill, 'function', `${face.name}.fill 应为函数`);
    }
  }

  // 高级分组挂载面（固定区渲染 / 单组渲染与回显 / 元数据）
  const adv = NS.parts.advanced;
  assert.ok(adv, 'NS.parts.advanced 应存在');
  assert.equal(typeof adv.renderHeader, 'function', 'advanced.renderHeader 应为函数');
  assert.equal(typeof adv.renderGroup, 'function', 'advanced.renderGroup 应为函数');
  assert.equal(typeof adv.fillGroup, 'function', 'advanced.fillGroup 应为函数');
  assert.equal(typeof adv.fillAll, 'function', 'advanced.fillAll 应为函数');
  assert.equal(typeof adv.groups, 'object', 'advanced.groups 应为元数据数组');
});
