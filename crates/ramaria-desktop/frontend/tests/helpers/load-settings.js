/**
 * tests/helpers/load-settings.js — 设置页模块加载辅助（node --test）
 *
 * 职责:
 * - 在单个 vm 沙箱中按 index.html 的 script 顺序加载 js/views/settings/ 下全部
 *   模块，返回视图单例、沙箱与 window 桩，供设置页相关测试共用。
 *
 * 说明:
 * - 模块加载清单（SETTINGS_MODULES）为单一来源，与 index.html 的 script 序列一致
 *   （settings-nav.test.js 依据本清单核对 index.html）；
 * - 基础桩：window / console（静默）/ RamariaRouter（钩子注册桩）；
 *   其余桩（RamariaApi / RamariaToast / document 等）由调用方经 extraSandbox 注入；
 * - RamariaRouter.getCurrentView 返回非 settings，避免自动初始化分支注册定时器
 *   导致测试进程挂起（与既有测试同口径）。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** 设置页模块根目录（frontend/js/views/settings） */
const SETTINGS_DIR = path.resolve(__dirname, '..', '..', 'js', 'views', 'settings');

/** 设置页模块加载顺序（与 index.html 的 script 序列一致） */
const SETTINGS_MODULES = [
  'shared.js',
  'nav.js',
  'sections/basic/backend.js',
  'sections/basic/embedding.js',
  'sections/basic/memory_injection.js',
  'sections/basic/session.js',
  'sections/basic/proactive.js',
  'sections/basic/channel.js',
  'sections/basic/privacy.js',
  'sections/basic/data_dir.js',
  'sections/basic/mcp.js',
  'sections/basic/data.js',
  'sections/basic/diagnostics.js',
  'sections/basic/about.js',
  'sections/advanced/groups.js',
  'index.js',
];

/**
 * 在 vm 沙箱中加载设置页全部模块。
 *
 * 参数:
 * - `extraSandbox`: 额外桩（合并进沙箱；同名键覆盖基础桩）。
 *
 * 返回:
 * - `{ view, sandbox, windowMock }`: 视图单例（window.RamariaSettingsView）、
 *   沙箱对象（window.RamariaSettingsNS.state / .parts 可读）与 window 桩。
 */
function loadSettingsView(extraSandbox) {
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    console: { log() {}, warn() {}, error() {} },
    RamariaRouter: {
      registerHook() {
        return function () {};
      },
      getCurrentView() {
        return 'chat';
      },
      setContentActions() {},
    },
  };

  if (extraSandbox) {
    for (const key of Object.keys(extraSandbox)) {
      sandbox[key] = extraSandbox[key];
    }
  }

  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  for (const rel of SETTINGS_MODULES) {
    const source = fs.readFileSync(path.join(SETTINGS_DIR, rel), 'utf8');
    vm.runInContext(source, sandbox, { filename: 'js/views/settings/' + rel });
  }

  return { view: windowMock.RamariaSettingsView, sandbox, windowMock };
}

module.exports = { loadSettingsView, SETTINGS_MODULES, SETTINGS_DIR };
