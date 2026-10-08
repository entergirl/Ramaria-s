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
 * - 图片占位符卡片（`[图片: 描述]` 卡片 / `[图片#hash]` 原文降级 / 混排分段 / XSS）
 * - sender_name 展示（群聊多说话人 / user 恒「你」/ 回退 persona 昵称）
 * - 导入前缀剥离（`[名字]` 前缀剥离；`[图片...]` 占位符不剥离）
 *
 * 运行: node --test "tests/*.test.js"
 *
 * 说明:
 * - 工具经 vm 沙箱加载，返回数组的原型属于沙箱 realm；断言前用 `split()`
 *   转回宿主数组，避免 deepStrictEqual 的原型比较失败。
 * - 消息气泡组件经 vm 沙箱 + 最小 DOM 桩加载，只走 `create()` 渲染路径；
 *   Markdown 使用真实实现（markdown.js），保证文本段渲染断言与线上一致。
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

/** Markdown 渲染器源码（frontend/js/utils/markdown.js） */
const MARKDOWN_PATH = path.resolve(
  __dirname, '..', 'js', 'utils', 'markdown.js'
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

/** 递归查找指定 tagName 的首个节点（断言未注入某类元素时用） */
function findByTag(root, tag) {
  const target = String(tag).toUpperCase();
  let found = null;
  function walk(node) {
    if (!node || found) return;
    if (node.tagName === target) { found = node; return; }
    const children = node.children || [];
    for (const child of children) walk(child);
  }
  walk(root);
  return found;
}

/** 递归拼接子树的展示文本（textContent 与 innerHTML 均参与，供 includes 断言） */
function flatText(root) {
  const parts = [];
  function walk(node) {
    if (!node) return;
    if (typeof node.textContent === 'string' && node.textContent) parts.push(node.textContent);
    if (typeof node.innerHTML === 'string' && node.innerHTML) parts.push(node.innerHTML);
    const children = node.children || [];
    for (const child of children) walk(child);
  }
  walk(root);
  return parts.join(' ');
}

/**
 * 在 vm 沙箱中加载 message-bubble.js（注入 DOM / Format / Store 最小桩，Markdown 用真实实现）。
 *
 * 参数:
 * - `options.personas`: RamariaStore.get('personas') 返回值（默认空数组），
 *   用于断言 persona 昵称回退路径。
 */
function loadMessageBubble(options) {
  const opts = options || {};
  const markdownSource = fs.readFileSync(MARKDOWN_PATH, 'utf8');
  const source = fs.readFileSync(MESSAGE_BUBBLE_PATH, 'utf8');
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    document: {
      createElement: createNode,
      createTextNode: (text) => ({ text: String(text) }),
    },
    RamariaFormat: { smartTime: (ts) => String(ts) },
    RamariaStore: { get: () => opts.personas || [] },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(markdownSource, sandbox, { filename: 'markdown.js' });
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

// =========================================================
// 图片占位符卡片（文本卡片通道：不加载真实图片文件）
// =========================================================

test('图片卡片：[图片: 猫] 渲染卡片、描述与 aria-hidden 图标', () => {
  const wrapper = messageBubble.create({
    id: 'img1',
    role: 'assistant',
    content: '[图片: 猫]',
  });

  const cards = collectByClass(wrapper, 'msg-image-card');
  assert.equal(cards.length, 1, '应渲染一个图片卡片');

  const descs = collectByClass(cards[0], 'msg-image-card-desc');
  assert.equal(descs.length, 1, '卡片应含描述元素');
  assert.equal(descs[0].textContent, '猫');

  const icons = collectByClass(cards[0], 'msg-image-card-icon');
  assert.equal(icons.length, 1, '卡片应含图标元素');
  assert.equal(icons[0].getAttribute('aria-hidden'), 'true');
});

test('图片卡片：描述含换行（注入形态允许）原样展示', () => {
  const wrapper = messageBubble.create({
    id: 'img2',
    role: 'assistant',
    content: '[图片: 图二\n第二行]',
  });

  const descs = collectByClass(wrapper, 'msg-image-card-desc');
  assert.equal(descs.length, 1);
  assert.equal(descs[0].textContent, '图二\n第二行');
});

test('图片卡片：混排时文本段 Markdown 渲染仍在，哈希占位符保留原文', () => {
  const wrapper = messageBubble.create({
    id: 'img3',
    role: 'assistant',
    content: '看这个 [图片: 猫] 还有 [图片#aabbccdd]，普通文本',
  });

  assert.equal(collectByClass(wrapper, 'msg-image-card').length, 1, '应仅渲染一个卡片');

  const textSegments = collectByClass(wrapper, 'msg-bubble-segment');
  assert.equal(textSegments.length, 2, '应保留卡片前后的两个文本段');
  assert.ok(
    textSegments[0].innerHTML.includes('<p>看这个</p>'),
    `文本段一应经 Markdown 渲染: ${textSegments[0].innerHTML}`
  );
  assert.ok(
    textSegments[1].innerHTML.includes('还有 [图片#aabbccdd]，普通文本'),
    `文本段二应经 Markdown 渲染且缺描述占位符保留原文: ${textSegments[1].innerHTML}`
  );
});

test('图片卡片：多图混排按顺序渲染多个卡片', () => {
  const wrapper = messageBubble.create({
    id: 'img4',
    role: 'assistant',
    content: '看 [图片: 猫] 和 [图片: 狗]',
  });

  const descs = collectByClass(wrapper, 'msg-image-card-desc');
  assert.equal(descs.length, 2, '应渲染两个卡片');
  assert.equal(descs[0].textContent, '猫');
  assert.equal(descs[1].textContent, '狗');
});

test('图片卡片：描述含方括号 / 未闭合的不完整形态保留原文', () => {
  const withBracket = messageBubble.create({
    id: 'img5',
    role: 'assistant',
    content: '看图 [图片: 猫[1]] 结束',
  });
  assert.equal(collectByClass(withBracket, 'msg-image-card').length, 0, '描述含方括号不渲染卡片');
  assert.ok(flatText(withBracket).includes('[图片: 猫[1]]'), flatText(withBracket));

  const unterminated = messageBubble.create({
    id: 'img6',
    role: 'assistant',
    content: '看图 [图片: 猫 结束',
  });
  assert.equal(collectByClass(unterminated, 'msg-image-card').length, 0, '未闭合占位符不渲染卡片');
  assert.ok(flatText(unterminated).includes('[图片: 猫 结束'), flatText(unterminated));
});

test('图片卡片：描述含 HTML 标签作为文本写入（不产生注入节点）', () => {
  const payload = '<img src=x onerror=alert(1)>';
  const wrapper = messageBubble.create({
    id: 'img7',
    role: 'assistant',
    content: '看 [图片: ' + payload + '] 结束',
  });

  const cards = collectByClass(wrapper, 'msg-image-card');
  assert.equal(cards.length, 1);

  const descs = collectByClass(wrapper, 'msg-image-card-desc');
  assert.equal(descs[0].textContent, payload, '描述应以纯文本承载');

  assert.equal(findByTag(wrapper, 'IMG'), null, '不得产生注入的图片元素');
});

// =========================================================
// sender_name 展示（群聊 / 导入消息的外部发送者）
// =========================================================

test('sender 展示：assistant + sender_name 用作标签与头像首字母', () => {
  const wrapper = messageBubble.create({
    id: 's1',
    role: 'assistant',
    content: '你好',
    sender_name: '张三',
    persona_uid: 'char-1',
  });

  const labels = collectByClass(wrapper, 'msg-bubble-label');
  assert.equal(labels.length, 1);
  assert.equal(labels[0].textContent, '张三');

  const avatars = collectByClass(wrapper, 'msg-bubble-avatar');
  assert.equal(avatars.length, 1);
  assert.equal(avatars[0].textContent, '张');
});

test('sender 展示：群聊多说话人各自显示 sender_name', () => {
  const first = messageBubble.create({
    id: 's2',
    role: 'assistant',
    content: '甲说话',
    sender_name: '张三',
  });
  const second = messageBubble.create({
    id: 's3',
    role: 'assistant',
    content: '乙说话',
    sender_name: '李四',
  });

  assert.equal(collectByClass(first, 'msg-bubble-label')[0].textContent, '张三');
  assert.equal(collectByClass(second, 'msg-bubble-label')[0].textContent, '李四');
});

test('sender 展示：user + sender_name 仍显示「你」且无头像', () => {
  const wrapper = messageBubble.create({
    id: 's4',
    role: 'user',
    content: '你好',
    sender_name: '张三',
  });

  assert.equal(collectByClass(wrapper, 'msg-bubble-label')[0].textContent, '你');
  assert.equal(collectByClass(wrapper, 'msg-bubble-avatar').length, 0);
});

test('sender 展示：sender_name 缺失回退 persona 昵称 / 「助手」', () => {
  const withPersona = loadMessageBubble({ personas: [{ uid: 'char-9', name: '小明' }] });
  const byPersona = withPersona.create({
    id: 's5',
    role: 'assistant',
    content: 'hi',
    persona_uid: 'char-9',
  });
  assert.equal(collectByClass(byPersona, 'msg-bubble-label')[0].textContent, '小明');

  const byFallback = messageBubble.create({
    id: 's6',
    role: 'assistant',
    content: 'hi',
  });
  assert.equal(collectByClass(byFallback, 'msg-bubble-label')[0].textContent, '助手');
});

test('sender 展示：头像背景色仍由 persona_uid 决定', () => {
  const withUid = messageBubble.create({
    id: 's7',
    role: 'assistant',
    content: 'hi',
    sender_name: '张三',
    persona_uid: 'char-1',
  });
  assert.match(collectByClass(withUid, 'msg-bubble-avatar')[0].style.backgroundColor, /^hsl\(/);

  const withoutUid = messageBubble.create({
    id: 's8',
    role: 'assistant',
    content: 'hi',
    sender_name: '张三',
  });
  assert.equal(
    collectByClass(withoutUid, 'msg-bubble-avatar')[0].style.backgroundColor,
    '#9ca3af',
    '无 persona_uid 时走灰色兜底（不取 sender_name 做色）'
  );
});

// =========================================================
// 导入前缀剥离（[名字] 前缀；[图片...] 占位符不剥离）
// =========================================================

test('前缀剥离：[张三] 你好 → 你好', () => {
  const wrapper = messageBubble.create({
    id: 'p1',
    role: 'user',
    content: '[张三] 你好',
  });

  const text = flatText(wrapper);
  assert.ok(text.includes('你好'), text);
  assert.ok(!text.includes('[张三]'), `昵称前缀应被剥离: ${text}`);
});

test('前缀剥离：[图片#aabbccdd] 行首占位符完整保留', () => {
  const wrapper = messageBubble.create({
    id: 'p2',
    role: 'user',
    content: '[图片#aabbccdd] 看这个',
  });

  const text = flatText(wrapper);
  assert.ok(text.includes('[图片#aabbccdd] 看这个'), text);
});

test('前缀剥离：[图片] / [图片: …] 行首占位符不剥离', () => {
  const noDesc = messageBubble.create({
    id: 'p3',
    role: 'user',
    content: '[图片] 看这个',
  });
  assert.ok(flatText(noDesc).includes('[图片] 看这个'), flatText(noDesc));

  const withDesc = messageBubble.create({
    id: 'p4',
    role: 'user',
    content: '[图片: 猫] 看这个',
  });
  assert.equal(collectByClass(withDesc, 'msg-image-card').length, 1);
  assert.equal(collectByClass(withDesc, 'msg-image-card-desc')[0].textContent, '猫');
  assert.ok(flatText(withDesc).includes('看这个'), flatText(withDesc));
});
