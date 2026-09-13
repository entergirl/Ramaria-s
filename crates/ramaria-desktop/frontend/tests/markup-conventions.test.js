/**
 * tests/markup-conventions.test.js — 前端标记约定回归（node --test）
 *
 * 目的:
 * - 锁定"密码输入框必须带语义化 autocomplete"约定：Chromium 对缺少 autocomplete 的
 *   password 字段会打印
 *   "[DOM] Input elements should have autocomplete attributes (suggested: \"new-password\")"，
 *   且密码管理器对该字段的行为未定义。
 * - 本项目两处 API Key 框（首次配置向导 / 设置页）均为"填写新密钥"语义，统一用
 *   `autocomplete="new-password"`（Chromium 官方建议值，见 Create Amazing Password Forms）。
 *
 * 说明:
 * - 视图标记以 JS 字符串拼接形式书写，跨行标签先把 `' + '` 连接合并再按标签整体匹配；
 * - 只约束 `type="password"` 的输入框；文本类字段的 autocomplete 不在本测试范围。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

/** 前端 js 根目录 */
const JS_DIR = path.resolve(__dirname, '..', 'js');

/** 递归收集目录下全部 .js 文件 */
function walkJsFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkJsFiles(full));
    else if (entry.name.endsWith('.js')) out.push(full);
  }
  return out;
}

/**
 * 合并相邻字符串拼接，便于按完整 HTML 标签匹配。
 *
 * 说明: 仅消除 `' + '` 这一拼接形态（含换行与缩进），不改变其余源码结构。
 */
function joinStringLiterals(source) {
  return source.replace(/'\s*\+\s*'/g, '');
}

test('密码输入框必须带 autocomplete="new-password"', () => {
  const violations = [];
  let passwordInputCount = 0;
  const pattern = /<input[^>]*type="password"[^>]*>/g;

  for (const file of walkJsFiles(JS_DIR)) {
    const joined = joinStringLiterals(fs.readFileSync(file, 'utf8'));
    let match;
    while ((match = pattern.exec(joined)) !== null) {
      passwordInputCount++;
      if (!/autocomplete="new-password"/.test(match[0])) {
        violations.push(`${path.relative(JS_DIR, file)}: ${match[0]}`);
      }
    }
  }

  assert.ok(passwordInputCount > 0, '未扫描到密码输入框（扫描逻辑失效）');
  assert.deepEqual(
    violations,
    [],
    `以下密码框缺少 autocomplete="new-password":\n${violations.join('\n')}`
  );
});
