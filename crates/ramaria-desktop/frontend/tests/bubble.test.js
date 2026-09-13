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
 *
 * 运行: node --test "tests/*.test.js"
 *
 * 说明:
 * - 工具经 vm 沙箱加载，返回数组的原型属于沙箱 realm；断言前用 `split()`
 *   转回宿主数组，避免 deepStrictEqual 的原型比较失败。
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
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
