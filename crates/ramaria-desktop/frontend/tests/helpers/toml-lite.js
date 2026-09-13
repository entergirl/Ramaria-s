/**
 * tests/helpers/toml-lite.js — 极简 TOML 读取器（仅供默认值一致性测试）
 *
 * 背景:
 * - 前端设置页默认值必须与后端配置模板 `config/default.toml`（及 `core/config.rs`）
 *   保持一致，否则"全量回写"会把用户配置覆盖成错误默认值（FE-03 类缺陷）。
 * - 测试环境不引入第三方依赖，故实现一个只覆盖本项目模板用法的子集解析器。
 *
 * 支持范围（严格匹配 `config/default.toml` 的写法）:
 * - 注释 `#`（字符串内不剥离）、空行、`[section]` 与 `[a.b.c]` 分组；
 * - `key = "字符串"` / `key = 123` / `key = 1.5` / `key = true|false` / `key = [数组]`；
 * - 数组仅支持单行、元素为字符串/数字/布尔。
 *
 * 不支持（遇到即抛错，避免"静默漏解析"掩盖模板问题）:
 * - 多行数组、内联表、日期、转义字符串、带点的裸键。
 *
 * 用法:
 *   const { parseToml } = require('./helpers/toml-lite.js');
 *   const cfg = parseToml(fs.readFileSync('config/default.toml', 'utf8'));
 */

'use strict';

/**
 * 剥离行注释（字符串字面量内的 `#` 不视为注释起点）。
 */
function stripComment(line) {
  let inString = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line.charAt(i);
    if (ch === '"') {
      inString = !inString;
    } else if (ch === '#' && !inString) {
      return line.slice(0, i);
    }
  }
  return line;
}

/**
 * 按路径逐级创建/获取分组对象。
 */
function ensurePath(root, segments) {
  let node = root;
  for (const seg of segments) {
    if (node[seg] === undefined) node[seg] = {};
    if (typeof node[seg] !== 'object' || Array.isArray(node[seg])) {
      throw new Error(`TOML 分组冲突: ${segments.join('.')}`);
    }
    node = node[seg];
  }
  return node;
}

/**
 * 解析单个值字面量。
 *
 * 参数:
 * - `text`: 值文本（已 trim、已去注释）。
 * - `lineNo`: 行号（报错定位用）。
 */
function parseValue(text, lineNo) {
  if (text === 'true') return true;
  if (text === 'false') return false;

  if (text.charAt(0) === '"') {
    if (text.charAt(text.length - 1) !== '"' || text.length < 2) {
      throw new Error(`TOML 第 ${lineNo} 行：字符串未闭合: ${text}`);
    }
    return text.slice(1, -1);
  }

  if (text.charAt(0) === '[') {
    if (text.charAt(text.length - 1) !== ']') {
      throw new Error(
        `TOML 第 ${lineNo} 行：数组须单行闭合（模板不支持多行数组）: ${text}`
      );
    }
    const inner = text.slice(1, -1).trim();
    if (inner === '') return [];
    const parts = inner.split(',');
    const out = [];
    for (const part of parts) {
      const item = part.trim();
      if (item === '') continue;
      out.push(parseValue(item, lineNo));
    }
    return out;
  }

  if (/^-?\d+$/.test(text)) return parseInt(text, 10);
  if (/^-?\d+\.\d+$/.test(text)) return parseFloat(text);

  throw new Error(`TOML 第 ${lineNo} 行：无法识别的值: ${text}`);
}

/**
 * 解析 TOML 文本为嵌套对象。
 *
 * 参数:
 * - `text`: TOML 源文本。
 *
 * 返回:
 * - 嵌套普通对象（分组为对象、键为值）。
 *
 * 异常:
 * - 语法无法识别时抛错并给出行号（宁可测试失败，也不静默漏键）。
 */
function parseToml(text) {
  const root = {};
  let current = root;

  const lines = String(text).split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const lineNo = i + 1;
    const line = stripComment(lines[i]).trim();
    if (line === '') continue;

    const sectionMatch = line.match(/^\[([^\]]+)\]$/);
    if (sectionMatch) {
      const segments = sectionMatch[1].split('.').map((s) => s.trim());
      current = ensurePath(root, segments);
      continue;
    }

    const kvMatch = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/);
    if (!kvMatch) {
      throw new Error(`TOML 第 ${lineNo} 行：无法解析: ${lines[i]}`);
    }
    current[kvMatch[1]] = parseValue(kvMatch[2].trim(), lineNo);
  }

  return root;
}

module.exports = { parseToml };
