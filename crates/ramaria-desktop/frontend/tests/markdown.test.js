/**
 * tests/markdown.test.js — RamariaMarkdown 渲染 + XSS 防护回归（node --test）
 *
 * 覆盖（前端纯逻辑测试）:
 * - 基础 Markdown：粗体 / 斜体 / 列表 / 代码块 / 链接
 * - XSS sanitize：<script> / onerror 事件属性被清除
 * - 代码块保护（内部不误解析）
 *
 * 运行: node --test tests/
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadUtil } = require('./helpers/load-util.js');

const md = loadUtil('markdown.js');

test('render: 粗体', () => {
  const html = md.render('**重要**内容');
  assert.ok(html.includes('<strong>重要</strong>'), `实际: ${html}`);
});

test('render: 斜体', () => {
  const html = md.render('*斜体*');
  assert.ok(html.includes('<em>斜体</em>'), `实际: ${html}`);
});

test('render: 无序列表', () => {
  const html = md.render('- 第一项\n- 第二项');
  assert.ok(html.includes('<li>'), `实际: ${html}`);
  assert.ok(html.includes('第一项'));
});

test('render: 链接带安全属性', () => {
  const html = md.render('[链接](https://example.com)');
  assert.ok(html.includes('rel="noopener noreferrer"'), `实际: ${html}`);
  assert.ok(html.includes('target="_blank"') || html.includes('target="_blank"'), `实际: ${html}`);
});

test('render: 代码块保护（块内标记不被误解析）', () => {
  const html = md.render('```\n**不是粗体**\n```');
  // 代码块内不应出现 <strong>
  assert.ok(!html.includes('<strong>'), `代码块内不应解析粗体，实际: ${html}`);
});

test('sanitize: 移除 script 标签（非白名单转义为文本）', () => {
  const safe = md.sanitize('<p>你好</p><script>alert(1)</script>');
  assert.ok(!safe.includes('<script'), `script 应被转义移除，实际: ${safe}`);
  assert.ok(safe.includes('你好'));
  // 非白名单标签被转义为 &lt;script&gt;（文本，不执行）
  assert.ok(safe.includes('&lt;script'), `script 应转义为文本，实际: ${safe}`);
});

test('sanitize: img（非白名单）整体转义为文本，onerror 无法生效', () => {
  // img 不在 ALLOWED_TAGS 白名单 → 整个标签被转义为文本（onerror 不执行）
  const safe = md.sanitize('<img src="x" onerror="alert(1)">');
  assert.ok(safe.includes('&lt;img'), `img 应转义为文本，实际: ${safe}`);
  // 转义后 onerror 存在于文本中但无 <img 可执行标签
  assert.ok(!safe.includes('<img'), `不应保留可执行 img 标签，实际: ${safe}`);
});

test('sanitize: iframe（非白名单）转义为文本', () => {
  const safe = md.sanitize('<iframe src="https://evil.com"></iframe>');
  assert.ok(!safe.includes('<iframe'), `iframe 不应保留为可执行标签，实际: ${safe}`);
  assert.ok(safe.includes('&lt;iframe'), `iframe 应转义为文本，实际: ${safe}`);
});

test('plainText: 剥离 Markdown 标记返回纯文本', () => {
  const text = md.plainText('**你好** 世界 [链接](https://x.com)');
  assert.ok(text.includes('你好'));
  assert.ok(text.includes('世界'));
  assert.ok(text.includes('链接'));
  assert.ok(!text.includes('**'), `粗体标记应剥离，实际: ${text}`);
  assert.ok(!text.includes(']('), `链接语法应剥离，实际: ${text}`);
});

test('plainText: 代码块整体移除', () => {
  const text = md.plainText('开头\n```\n内部代码\n```\n结尾');
  assert.ok(text.includes('开头'));
  assert.ok(text.includes('结尾'));
  assert.ok(!text.includes('内部代码'), `代码块内容应移除，实际: ${text}`);
});