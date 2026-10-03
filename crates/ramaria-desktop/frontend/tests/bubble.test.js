/**
 * tests/bubble.test.js — 多气泡切分（`||` 契约）纯函数回归（node --test）
 *
 * 覆盖:
 * - 单段 / 两段 / 三段切分与 trim
 * - 空段丢弃（首尾/连续分隔符）
 * - 无分隔符单段、空内容、非字符串入参
 * - 豁免区不切分（围栏代码块 / 行内代码 / 链接目标 / 裸 URL，CR2-COR-011）
 * - streamDisplay 流式展示（豁免区外 `||` → 换行）
 * - 分隔符常量导出
 * - 消息气泡主动标识（is_proactive → `.msg-bubble-proactive` 存在性）
 *
 * 运行: node --test "tests/*.test.js"
 *
 * 说明:
 * - 工具经 vm 沙箱加载，返回数组的原型属于沙箱 realm；断言前用 `split()`
 *   转回宿主数组，避免 deepStrictEqual 的原型比较失败。
 * - 消息气泡组件经 vm 沙箱 + 最小 DOM 桩加载，只走 `create()` 渲染路径。
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { loadUtil } = require('./helpers/load-util.js');

const bubble = loadUtil('bubble.js');

/** 调用被测函数并把返回值转回宿主 realm 的数组 */
function split(content) {
  return Array.from(bubble.splitBubbles(content));
}

test('splitBubbles: 无分隔符返回单段', () => {
  assert.deepEqual(split('先去吃饭'), ['先去吃饭']);
});

test('splitBubbles: 单个分隔符切两段', () => {
  assert.deepEqual(split('先去吃饭||饿着扛可不行'), ['先去吃饭', '饿着扛可不行']);
});

test('splitBubbles: 多个分隔符切多段', () => {
  assert.deepEqual(split('甲||乙||丙'), ['甲', '乙', '丙']);
});

test('splitBubbles: 段两侧空白被 trim', () => {
  assert.deepEqual(split('  甲 || 乙  '), ['甲', '乙']);
});

test('splitBubbles: 首尾与连续分隔符的空段被丢弃', () => {
  assert.deepEqual(split('||甲||||乙||'), ['甲', '乙']);
});

test('splitBubbles: 纯分隔符返回空数组', () => {
  assert.deepEqual(split('||||'), []);
});

test('splitBubbles: 空内容返回空数组', () => {
  assert.deepEqual(split(''), []);
});

test('splitBubbles: 非字符串入参返回空数组', () => {
  assert.deepEqual(split(null), []);
  assert.deepEqual(split(undefined), []);
  assert.deepEqual(split(123), []);
});

test('splitBubbles: 正文内换行不参与切分', () => {
  assert.deepEqual(split('第一行\n第二行||下一段'), ['第一行\n第二行', '下一段']);
});

// ── CR2-COR-011：豁免区（代码块 / 行内代码 / 链接与 URL）不切分 ──

test('splitBubbles: 围栏代码块内的 || 不切分', () => {
  assert.deepEqual(split('```\nconst a = x || y;\n```'), ['```\nconst a = x || y;\n```']);
});

test('splitBubbles: 围栏代码块外仍正常切分', () => {
  assert.deepEqual(
    split('```\na || b\n```||下一段'),
    ['```\na || b\n```', '下一段']
  );
});

test('splitBubbles: 行内代码内的 || 不切分', () => {
  assert.deepEqual(split('用 `a||b` 表示或||就是这样'), ['用 `a||b` 表示或', '就是这样']);
});

test('splitBubbles: 链接目标与裸 URL 内的 || 不切分', () => {
  assert.deepEqual(split('[说明](https://x.test/a||b)'), ['[说明](https://x.test/a||b)']);
  assert.deepEqual(split('见 https://x.test/a||b 结尾'), ['见 https://x.test/a||b 结尾']);
});

test('splitBubbles: 纯分隔符返回空数组（调用方回退原文单泡）', () => {
  assert.deepEqual(split('||'), []);
  assert.deepEqual(split('||||'), []);
});

test('splitBubbles: 空白内容返回空数组', () => {
  assert.deepEqual(split('   '), []);
});

test('streamDisplay: 正常分隔符替换为换行', () => {
  assert.equal(bubble.streamDisplay('甲||乙'), '甲\n乙');
  assert.equal(bubble.streamDisplay('甲 || 乙 || 丙'), '甲 \n 乙 \n 丙');
});

test('streamDisplay: 豁免区内的 || 保持原样', () => {
  assert.equal(bubble.streamDisplay('```\na||b\n```'), '```\na||b\n```');
  assert.equal(bubble.streamDisplay('用 `a||b`||下一段'), '用 `a||b`\n下一段');
});

test('streamDisplay: 空内容返回空串', () => {
  assert.equal(bubble.streamDisplay(''), '');
  assert.equal(bubble.streamDisplay(null), '');
});

test('SEPARATOR: 常量为 ||', () => {
  assert.equal(bubble.SEPARATOR, '||');
});

// =========================================================
// 消息气泡主动标识（dom 桩加载 message-bubble 组件）
// =========================================================

/** 消息气泡组件源码（frontend/js/components/message-bubble.js） */
const MESSAGE_BUBBLE_PATH = path.resolve(
  __dirname, '..', 'js', 'components', 'message-bubble.js'
);

/** 最小 DOM 节点桩：满足 create() 渲染路径（createElement / appendChild / setAttribute） */
function createNode(tag) {
  const node = {
    tagName: String(tag).toUpperCase(),
    children: [],
    attributes: {},
    className: '',
    textContent: '',
    innerHTML: '',
    style: {},
    appendChild(child) {
      this.children.push(child);
      return child;
    },
    setAttribute(name, value) {
      this.attributes[name] = String(value);
    },
    getAttribute(name) {
      return Object.prototype.hasOwnProperty.call(this.attributes, name)
        ? this.attributes[name]
        : null;
    },
    removeChild(child) {
      const idx = this.children.indexOf(child);
      if (idx !== -1) this.children.splice(idx, 1);
    },
    addEventListener() {},
  };
  return node;
}

/** 递归收集 className 精确命中的节点 */
function collectByClass(root, className) {
  const out = [];
  function walk(node) {
    if (!node) return;
    const cls = typeof node.className === 'string' ? node.className : '';
    if (cls.split(/\s+/).indexOf(className) !== -1) out.push(node);
    const children = node.children || [];
    for (const child of children) walk(child);
  }
  walk(root);
  return out;
}

/** 在 vm 沙箱中加载 message-bubble.js（注入 DOM / Markdown / Format / Store 最小桩） */
function loadMessageBubble() {
  const source = fs.readFileSync(MESSAGE_BUBBLE_PATH, 'utf8');
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    document: {
      createElement: createNode,
      createTextNode: (text) => ({ text: String(text) }),
    },
    RamariaMarkdown: { render: (text) => String(text) },
    RamariaFormat: { smartTime: (ts) => String(ts) },
    RamariaStore: { get: () => [] },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'message-bubble.js' });

  const component = windowMock.RamariaMessageBubble;
  if (!component) throw new Error('message-bubble.js 未暴露 RamariaMessageBubble');
  return component;
}

const messageBubble = loadMessageBubble();

test('消息气泡：is_proactive=true 显示「主动」标识', () => {
  const wrapper = messageBubble.create({
    id: 'm1',
    role: 'assistant',
    content: '在吗？突然想起你说过的事。',
    created_at: 1759459200000,
    is_proactive: true,
  });

  const tags = collectByClass(wrapper, 'msg-bubble-proactive');
  assert.equal(tags.length, 1, '应渲染一个主动标识');
  assert.equal(tags[0].textContent, '主动');
  assert.equal(tags[0].title, '这条消息由 Ramaria 主动发起');
});

test('消息气泡：is_proactive 缺省 / false 不显示标识', () => {
  const explicitFalse = messageBubble.create({
    id: 'm2',
    role: 'assistant',
    content: '普通消息',
    is_proactive: false,
  });
  assert.equal(collectByClass(explicitFalse, 'msg-bubble-proactive').length, 0);

  const missing = messageBubble.create({
    id: 'm3',
    role: 'assistant',
    content: '普通消息',
  });
  assert.equal(collectByClass(missing, 'msg-bubble-proactive').length, 0);
});
