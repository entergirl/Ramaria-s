/**
 * tests/memory-time.test.js — 记忆视图时间字段口径回归（node --test）
 *
 * 目的（时间语义：发生时间为第一公民）:
 * - L1 卡片时间取发生时间（`item.occurred_at`）——导入的历史记忆按真实发生时刻显示；
 * - L2 详情"时间"行取事件开始时间（`item.start`，与后端视图 / CLI 口径统一）；
 * - 上述两个时间显示位不得回退到入库时间 `created_at`。
 *
 * 说明:
 * - memory.js 为浏览器 IIFE 视图（渲染函数私有、依赖 DOM 与 Store），
 *   与 markup-conventions.test.js 同法做源码级口径锁定：合并 `' + '` 拼接后按片段整体匹配；
 * - 其余 `created_at` 消费位（如 L3 标签时间）语义未变，不做约束。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

/** 记忆视图源码（frontend/js/views/memory.js） */
const MEMORY_VIEW_PATH = path.resolve(__dirname, '..', 'js', 'views', 'memory.js');

/** 合并相邻字符串拼接，便于按完整片段匹配。 */
function joinStringLiterals(source) {
  return source.replace(/'\s*\+\s*'/g, '');
}

const source = joinStringLiterals(fs.readFileSync(MEMORY_VIEW_PATH, 'utf8'));

test('L1 卡片时间取发生时间（occurred_at）', () => {
  assert.ok(
    source.includes('RamariaFormat.smartTime(item.occurred_at)'),
    'L1 卡片时间应使用 item.occurred_at（发生时间）'
  );
});

test('L2 详情时间取事件开始时间（start）', () => {
  assert.ok(
    source.includes('RamariaFormat.smartTime(item.start)'),
    'L2 详情时间应使用 item.start（事件开始时间）'
  );
});

test('记忆视图时间显示位不回落入库时间（created_at）', () => {
  assert.ok(
    !/smartTime\(item\.created_at\)/.test(source),
    'L1/L2 时间显示位不得使用 item.created_at（入库时间仅供审计）'
  );
});
