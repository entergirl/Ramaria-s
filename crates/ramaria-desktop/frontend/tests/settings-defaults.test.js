/**
 * tests/settings-defaults.test.js — 设置页高级字段默认值一致性回归（node --test）
 *
 * 目的（FE-03 / FE-06 验收）:
 * - 前端 `_ADVANCED_GROUPS` 的默认值与后端配置模板 `config/default.toml` 逐键比对：
 *   只要"前端默认值 ≠ 模板默认值"就失败，防止全量回写把用户配置覆盖成错误值；
 * - 显式锁定基线参数（utt 切分 10/80）与 v2.0 新增项
 *   （`injection_budget.order`、`event_extraction.degraded_confidence_enabled`）的
 *   存在性、类型与默认值。
 *
 * 说明:
 * - settings.js 是浏览器 IIFE 视图，本测试用 vm 注入最小桩（Router/window/console）
 *   加载它，只读取 `getAdvancedGroups()` 暴露的元数据快照，不触发任何 DOM 操作；
 * - 模板未收录的键（如待 CFG-01 补齐的分组）跳过比对，不做"反向要求模板"的断言，
 *   避免测试越权约束配置模板的收录范围。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { parseToml } = require('./helpers/toml-lite.js');

/** 设置页视图源码（frontend/js/views/settings.js） */
const SETTINGS_PATH = path.resolve(__dirname, '..', 'js', 'views', 'settings.js');

/** 后端配置模板（main/config/default.toml） */
const DEFAULT_TOML_PATH = path.resolve(
  __dirname, '..', '..', '..', '..', 'config', 'default.toml'
);

/**
 * 在 vm 沙箱中加载 settings.js，返回其全局单例。
 *
 * 桩说明:
 * - `RamariaRouter`：init() 只注册钩子；`getCurrentView()` 返回非 settings，
 *   避免自动初始化分支注册定时器导致测试进程挂起。
 * - `window`：收集 defineProperty 暴露的 `RamariaSettingsView`。
 * - 不注入 `document`：本测试不触碰 DOM（getAdvancedGroups 为纯数据读取）。
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
  if (!view || typeof view.getAdvancedGroups !== 'function') {
    throw new Error('settings.js 未暴露 getAdvancedGroups()');
  }
  return view;
}

/** 按点分路径读取嵌套对象值 */
function getByPath(root, segments) {
  let node = root;
  for (const seg of segments) {
    if (node === undefined || node === null) return undefined;
    node = node[seg];
  }
  return node;
}

/**
 * 计算字段在完整配置中的绝对路径（与设置页 `_advConfigPath` 同口径）。
 *
 * 说明: 组元数据里字段 `path` 为组内相对路径，`section` 为配置段前缀；
 * 缺前缀会读写到错误的键（读取恒 undefined、保存静默无效），此处以此口径做回归锁定。
 */
function resolveFieldPath(group, field) {
  const section = Array.isArray(group.section) ? group.section : [];
  return section.concat(field.path);
}

/** 把高级配置组展开为 { '绝对点分路径': field } 映射 */
function flattenFields(groups) {
  const map = {};
  for (const group of groups) {
    for (const field of group.fields) {
      map[resolveFieldPath(group, field).join('.')] = field;
    }
  }
  return map;
}

const settingsView = loadSettingsView();
// 经 JSON 往返转换为宿主 realm 对象/数组：vm 沙箱返回值的原型属于沙箱 realm，
// 直接用 deepStrictEqual 会因原型不同而误报（与 bubble.test.js 同一注意事项）
const groups = JSON.parse(JSON.stringify(settingsView.getAdvancedGroups()));
const fields = flattenFields(groups);
const templateConfig = parseToml(fs.readFileSync(DEFAULT_TOML_PATH, 'utf8'));

test('高级字段默认值与 config/default.toml 逐键一致', () => {
  const mismatches = [];

  for (const [key, field] of Object.entries(fields)) {
    const templateValue = getByPath(templateConfig, key.split('.'));
    if (templateValue === undefined) continue;  // 模板未收录该键（如待补齐分组）

    const same = Array.isArray(field.def)
      ? JSON.stringify(field.def) === JSON.stringify(templateValue)
      : field.def === templateValue;

    if (!same) {
      mismatches.push(
        `${key}: 前端 def=${JSON.stringify(field.def)} vs default.toml=${JSON.stringify(templateValue)}`
      );
    }
  }

  assert.deepEqual(mismatches, [], `默认值漂移（会写错用户配置）:\n${mismatches.join('\n')}`);
});

test('FE-03: utt 切分参数与基线一致（10 分钟 / 80 条）', () => {
  assert.ok(fields['utt.theta_gap_minutes'], '应存在 utt.theta_gap_minutes');
  assert.equal(fields['utt.theta_gap_minutes'].def, 10);
  assert.ok(fields['utt.max_msgs_per_block'], '应存在 utt.max_msgs_per_block');
  assert.equal(fields['utt.max_msgs_per_block'].def, 80);
});

test('FE-06: 降级事件动态置信度开关存在且默认开启', () => {
  const field = fields['event_extraction.degraded_confidence_enabled'];
  assert.ok(field, '应存在 event_extraction.degraded_confidence_enabled');
  assert.equal(field.type, 'bool');
  assert.equal(field.def, true);
});

test('FE-06: 注入优先级 order 项存在且默认顺序正确', () => {
  const field = fields['injection_budget.order'];
  assert.ok(field, '应存在 injection_budget.order');
  assert.equal(field.type, 'order');
  assert.deepEqual(field.def, ['rag', 'behavior', 'knowledge', 'style', 'memory']);

  // 每个默认通道都必须有对应选项（否则 UI 无法完整重建列表）
  const optionValues = field.options.map((o) => o.value);
  for (const slot of field.def) {
    assert.ok(optionValues.includes(slot), `order 默认通道 ${slot} 缺少选项标签`);
  }
});

test('FE-06: order 选项与 core InjectionSlot 全量对齐', () => {
  const field = fields['injection_budget.order'];
  const optionValues = field.options.map((o) => o.value).sort();
  // ramaria-core 的 InjectionSlot（serde lowercase）：rag / behavior / knowledge / style / memory
  assert.deepEqual(optionValues, ['behavior', 'knowledge', 'memory', 'rag', 'style']);
});

test('路径解析：字段绝对路径 = section + 组内相对路径', () => {
  // 回归锁定：utt 组字段相对路径为 ['theta_gap_minutes']，模板中不存在根键
  // theta_gap_minutes，只有 utt.theta_gap_minutes；一旦 section 前缀丢失，
  // 高级表单会"读不到实际值、保存写错键（静默无效）"。
  const utt = groups.find((g) => g.key === 'utt');
  assert.ok(utt, '应存在 utt 组');
  assert.deepEqual(utt.section, ['utt']);
  assert.equal(getByPath(templateConfig, ['theta_gap_minutes']), undefined);
  assert.equal(getByPath(templateConfig, ['utt', 'theta_gap_minutes']), 10);

  // l1-progressive / inference-upgrade 使用多级段路径，同样必须拼出完整键
  const progressive = groups.find((g) => g.key === 'l1-progressive');
  assert.ok(progressive, '应存在 l1-progressive 组');
  assert.equal(
    resolveFieldPath(progressive, progressive.fields[0]).join('.'),
    'l1.progressive.enabled'
  );
  assert.equal(
    getByPath(templateConfig, ['l1', 'progressive', 'enabled']),
    true
  );

  const upgrade = groups.find((g) => g.key === 'inference-upgrade');
  assert.ok(upgrade, '应存在 inference-upgrade 组');
  assert.equal(
    resolveFieldPath(upgrade, upgrade.fields[0]).join('.'),
    'inference.upgrade.cross_version_threshold_085'
  );
});

test('高级字段元数据形态合法（分组完整 / 路径唯一 / 类型受控 / 默认值类型匹配）', () => {
  const seenPaths = new Set();
  const allowedTypes = ['number', 'bool', 'whitelist', 'order'];
  const seenKeys = new Set();

  for (const group of groups) {
    assert.ok(group.key && !seenKeys.has(group.key), `分组 key 缺失或重复: ${group.key}`);
    seenKeys.add(group.key);
    assert.ok(Array.isArray(group.section), `${group.key}: 缺少 section 段路径`);
    assert.ok(group.title && group.desc, `${group.key}: 缺少标题/说明`);
    assert.ok(Array.isArray(group.fields) && group.fields.length > 0, `${group.key}: 缺少字段`);

    for (const field of group.fields) {
      const key = resolveFieldPath(group, field).join('.');
      assert.ok(!seenPaths.has(key), `字段路径重复（配置键/DOM id 会冲突）: ${key}`);
      seenPaths.add(key);
      assert.ok(
        field.path.length > 0 && group.section.length + field.path.length > 0,
        `${key}: 路径非法`
      );

      assert.ok(Array.isArray(field.path) && field.path.length > 0, `${key}: path 非法`);
      assert.ok(allowedTypes.includes(field.type), `${key}: 未知类型 ${field.type}`);
      assert.ok(field.label && field.hint, `${key}: 缺少 label/hint`);

      if (field.type === 'number') {
        assert.equal(typeof field.def, 'number', `${key}: number 默认值应为数字`);
      } else if (field.type === 'bool') {
        assert.equal(typeof field.def, 'boolean', `${key}: bool 默认值应为布尔`);
      } else {
        assert.ok(Array.isArray(field.def), `${key}: ${field.type} 默认值应为数组`);
        assert.ok(Array.isArray(field.options) && field.options.length > 0, `${key}: 缺少 options`);
      }
    }
  }
});

// =========================================================
// MCP 接入面板（v2.1 M5）：字段默认值与 config/default.toml 逐键一致
// =========================================================

if (typeof settingsView.getMcpFields !== 'function') {
  throw new Error('settings.js 未暴露 getMcpFields()（MCP 面板元数据缺失）');
}

// 以伪组包装（section 前缀 + 字段元数据），复用高级组的路径解析口径
const mcpFields = flattenFields([
  { key: 'mcp', section: ['mcp'], fields: settingsView.getMcpFields() },
]);

test('MCP 面板字段默认值与 config/default.toml 逐键一致', () => {
  const mismatches = [];

  for (const [key, field] of Object.entries(mcpFields)) {
    const templateValue = getByPath(templateConfig, key.split('.'));
    if (templateValue === undefined) {
      // 与高级组不同：MCP 面板字段必须全部收录于模板（面板保存走全量回写）
      mismatches.push(`${key}: default.toml 未收录该键`);
      continue;
    }

    const same = Array.isArray(field.def)
      ? JSON.stringify(field.def) === JSON.stringify(templateValue)
      : field.def === templateValue;

    if (!same) {
      mismatches.push(
        `${key}: 前端 def=${JSON.stringify(field.def)} vs default.toml=${JSON.stringify(templateValue)}`
      );
    }
  }

  assert.deepEqual(mismatches, [], `MCP 面板默认值漂移:\n${mismatches.join('\n')}`);
});

test('MCP 面板：七键齐全且字段形态合法', () => {
  const expected = [
    'mcp.enabled',
    'mcp.allow_ingest',
    'mcp.allow_seal',
    'mcp.allowed_personas',
    'mcp.allow_raw_text',
    'mcp.max_items',
    'mcp.max_chars',
  ];
  assert.deepEqual(Object.keys(mcpFields).sort(), expected.slice().sort());

  const allowedTypes = ['bool', 'number', 'string-list'];
  for (const [key, field] of Object.entries(mcpFields)) {
    assert.ok(allowedTypes.includes(field.type), `${key}: 未知类型 ${field.type}`);
    assert.ok(field.label && field.hint, `${key}: 缺少 label/hint`);

    if (field.type === 'bool') {
      assert.equal(typeof field.def, 'boolean', `${key}: bool 默认值应为布尔`);
    } else if (field.type === 'number') {
      assert.equal(typeof field.def, 'number', `${key}: number 默认值应为数字`);
      assert.ok(field.min !== undefined && field.max !== undefined, `${key}: 缺少取值边界`);
    } else {
      assert.ok(Array.isArray(field.def), `${key}: string-list 默认值应为数组`);
    }
  }
});

test('MCP 面板：白名单默认全部人格可见', () => {
  // 经 JSON 往返归一：vm 沙箱数组的原型与宿主 realm 不同，直接 deepEqual 会误报
  const def = JSON.parse(JSON.stringify(mcpFields['mcp.allowed_personas'].def));
  assert.deepEqual(def, ['*']);
});
