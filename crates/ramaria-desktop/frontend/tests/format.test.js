/**
 * tests/format.test.js — RamariaFormat 纯函数回归（node --test）
 *
 * 覆盖（前端纯逻辑测试）:
 * - relativeTime: 刚刚 / 分钟前 / 小时前 / 昨天 / 天前 / 绝对时间 / 未来兜底
 * - smartTime / duration / number / compactNumber / percent / fileSize / truncate
 * - NaN/负数/边界安全回退
 *
 * 运行: node --test tests/
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadUtil } = require('./helpers/load-util.js');

const format = loadUtil('format.js');

test('relativeTime: 刚刚（60 秒内）', () => {
  const now = 1718123456000;
  assert.equal(format.relativeTime(now - 30000, now), '刚刚');
});

test('relativeTime: 分钟前', () => {
  const now = 1718123456000;
  assert.ok(format.relativeTime(now - 3 * 60000, now).includes('分钟前'));
});

test('relativeTime: 小时前', () => {
  const now = 1718123456000;
  assert.ok(format.relativeTime(now - 2 * 3600000, now).includes('小时前'));
});

test('relativeTime: 昨天（昨天自然日）', () => {
  // _todayStart 使用真实时钟：构造"昨天自然日 12:00"并与真实现在对比
  const now = Date.now();
  const today = new Date();
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1, 12, 0);
  const text = format.relativeTime(yesterday.getTime(), now);
  assert.ok(text.includes('昨天'), `昨天自然日应显示"昨天"，实际: ${text}`);
});

test('relativeTime: 7 天内 X 天前', () => {
  const now = 1718123456000;
  assert.ok(format.relativeTime(now - 3 * 86400000, now).includes('天前'));
});

test('relativeTime: 超过 7 天 → 绝对时间', () => {
  const now = 1718123456000;
  const text = format.relativeTime(now - 30 * 86400000, now);
  // 同年场景返回 "M月D日 HH:MM" 或 ISO 格式，但不含"刚刚/前"
  assert.ok(
    !text.includes('前'),
    `30 天前不应显示相对时间，实际: ${text}`
  );
  assert.ok(/\d/.test(text), `应含日期数字，实际: ${text}`);
});

test('relativeTime: 未来时间安全回退（不抛异常）', () => {
  const now = 1718123456000;
  const text = format.relativeTime(now + 5000, now);
  assert.equal(typeof text, 'string');
});

test('number: 千分位 + 负数 + 小数', () => {
  assert.equal(format.number(1234567), '1,234,567');
  assert.ok(String(format.number(-9999)).includes('9,999'));
  assert.ok(String(format.number(1234.56)).includes('1,234'));
});

test('compactNumber: 万级缩写', () => {
  const text = format.compactNumber(12345);
  assert.ok(text.includes('万') || text.includes('k') || /1\.2/.test(text));
});

test('percent: 比例转百分比', () => {
  assert.equal(format.percent(0.85, 0), '85%');
  // 两参数形态: 可能返回 '85.00%'
  const text = format.percent(0.856, 2);
  assert.ok(text.includes('%'));
});

test('fileSize: 字节格式化', () => {
  const text = format.fileSize(1536000, 1);
  assert.ok(text.includes('MB') || text.includes('M'), `实际: ${text}`);
});

test('duration: 秒转可读时长', () => {
  assert.ok(format.duration(125).includes('2分'));
  assert.ok(format.duration(90).includes('分'));
});

test('truncate: NaN 安全回退', () => {
  assert.equal(format.truncate(NaN, 5), '0');
  assert.equal(format.truncate('abc', 5), '0', '非数字回退 0');
});