/**
 * tests/helpers/load-util.js — 在 Node 环境加载浏览器 IIFE 工具（RAMARIA 纯函数）
 *
 * 背景:
 * - format.js / markdown.js / dom.js 通过 `var RamariaX = (function(){})()` + 
 *   `Object.defineProperty(window, ...)` 暴露全局单例，无 CommonJS 导出。
 * - 本助手使用 Node `vm` 模块在独立上下文执行源码，并注入模拟 `window`，
 *   从而在不修改产品代码的前提下对纯函数做 node --test 回归。
 *
 * 用法:
 *   var RamariaFormat = loadUtil('format.js');
 *   RamariaFormat.relativeTime(ts);
 *
 * 设计特点:
 * - vm 上下文仅注入 window/globalThis/document（document 为空壳，markdown 用不到）。
 * - 每次调用独立上下文（测试隔离）。
 * - 路径解析相对本文件（tests/helpers/ → js/utils/）。
 */

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** js/utils 目录（相对本文件的位置：tests/helpers/load-util.js → ../../js/utils） */
const UTILS_DIR = path.resolve(__dirname, '..', '..', 'js', 'utils');

/**
 * 在独立 VM 上下文加载一个 utils 文件，返回其全局单例。
 *
 * 参数:
 * - `fileName`: utils 目录下文件名，如 'format.js'。
 *
 * 返回:
 * - 该文件暴露的全局单例（RamariaFormat / RamariaMarkdown / RamariaEscape）。
 *
 * 说明:
 * - 模拟 window 对象收集 defineProperty 写入的单例。
 * - 若单例未定义（加载失败）抛出明确错误。
 */
function loadUtil(fileName) {
  const filePath = path.join(UTILS_DIR, fileName);
  if (!fs.existsSync(filePath)) {
    throw new Error(`工具文件不存在: ${filePath}`);
  }
  const source = fs.readFileSync(filePath, 'utf8');

  // 模拟浏览器 window：收集 defineProperty(value=...) 写入的全局单例
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    globalThis: windowMock,
    // markdown/format 不触碰真实 DOM；提供空壳以防未来误用
    document: undefined,
    console: console,
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: fileName });

  // 从 window 的 defineProperty 收集单例
  //（defineProperty 默认 enumerable:false，需用 getOwnPropertyNames）
  const keys = Object.getOwnPropertyNames(windowMock);
  if (keys.length === 0) {
    // 兜底：若实现改为直接赋值，读 sandbox 顶层同名 var
    throw new Error(`未在 window 上找到单例: ${fileName}（keys=${keys.join(',')}）`);
  }
  // 返回第一个收集到的单例（utils 文件各自只暴露一个）
  return windowMock[keys[0]];
}

module.exports = { loadUtil };