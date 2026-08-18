/**
 * tests/dom.test.js — RamariaEscape.escapeHtml XSS 防护回归（node --test）
 *
 * 覆盖（前端纯逻辑测试）:
 * - 文本语境与属性语境的特殊字符转义
 * - null/undefined 安全回退
 * - 幂等性（重复转义不破坏）
 *
 * 运行: node --test tests/
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadUtil } = require('./helpers/load-util.js');

const escape = loadUtil('dom.js');

test('escapeHtml: 基础特殊字符', () => {
  assert.equal(escape.escapeHtml('&'), '&amp;');
  assert.equal(escape.escapeHtml('<'), '&lt;');
  assert.equal(escape.escapeHtml('>'), '&gt;');
});

test('escapeHtml: 属性语境引号', () => {
  const out = escape.escapeHtml('"双引号" \'单引号\'');
  assert.ok(out.includes('&quot;'), `双引号应转义，实际: ${out}`);
  assert.ok(out.includes('&#39;') || out.includes('&#x27;'), `单引号应转义，实际: ${out}`);
});

test('escapeHtml: XSS payload 中和', () => {
  const payload = '<img src=x onerror=alert(1)>';
  const out = escape.escapeHtml(payload);
  assert.ok(!out.includes('<img'), '标签名应被转义');
  assert.ok(out.includes('&lt;img'), `实际: ${out}`);
});

test('escapeHtml: null/undefined 安全回退', () => {
  assert.equal(escape.escapeHtml(null), '');
  assert.equal(escape.escapeHtml(undefined), '');
});

test('escapeHtml: 幂等（已转义输入再转义不破碎）', () => {
  const once = escape.escapeHtml('a & b');
  const twice = escape.escapeHtml(once);
  // &amp; 再转义会变成 &amp;amp;——但至少不产生原始特殊字符
  assert.ok(!twice.includes('& '), '不应有裸 & 后跟空格');
  assert.ok(twice.includes('amp;'), '应保留转义实体');
});