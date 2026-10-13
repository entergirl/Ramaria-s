/**
 * tests/settings-channel.test.js — 设置页「社交平台接入」回归（node --test）
 *
 * 覆盖:
 * - 面板字段元数据与高级设置页 channel 组逐键一致（默认值防双处漂移）
 * - 连接状态展示视图（四态 + 未知值兜底）
 * - 绑定行视图模型（通道标签 / 私聊群聊标签 / 字段归一 / 异常数据跳过）
 * - 凭据保留键映射（qq / telegram / 未知平台）
 * - 表单值合并完整配置（纯函数：不修改入参、缺失组补建、未传值不写入）
 * - 凭据写入：经 updateSetting 保留键提交；未知平台 / 空值拒绝
 * - 凭据保存处理器：空输入仅提示不写入；成功写入后清空输入框
 *
 * 说明:
 * - 设置页为浏览器 IIFE 多模块结构（js/views/settings/）；本测试经
 *   tests/helpers/load-settings.js 在 vm 沙箱中按序加载全部模块，仅调用
 *   `RamariaSettingsView` 暴露的纯函数与处理器，不触发真实渲染流程；
 * - vm 沙箱返回值的原型属于沙箱 realm，断言前用 JSON 往返归一。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { loadSettingsView: loadSettingsModules } = require('./helpers/load-settings.js');

/** JSON 往返归一（vm realm 对象 → 宿主 realm） */
function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

/**
 * 加载设置页（js/views/settings/ 全部模块按序进入同一 vm 沙箱），返回视图单例与调用记录。
 *
 * 参数:
 * - `opts.elements`: document.getElementById 的 id → 元素桩映射（缺省 null）；
 * - `opts.updateSettingFails`: 非空时 updateSetting 抛出该消息。
 *
 * 返回:
 * - `view`: RamariaSettingsView；
 * - `calls`: { updateSetting, toasts } 调用记录。
 */
function loadSettingsView(opts) {
  const options = opts || {};
  const calls = { updateSetting: [], toasts: [] };

  const { view } = loadSettingsModules({
    RamariaEscape: {
      escapeHtml(str) {
        if (str === undefined || str === null) return '';
        return String(str)
          .replace(/&/g, '&amp;')
          .replace(/</g, '&lt;')
          .replace(/>/g, '&gt;')
          .replace(/"/g, '&quot;')
          .replace(/'/g, '&#39;');
      },
    },
    RamariaToast: {
      show(type, title, detail) {
        calls.toasts.push({ type, title, detail });
      },
    },
    RamariaApi: {
      channel: {
        async getStatus() {
          return {
            enabled: true,
            qqEnabled: true,
            telegramEnabled: false,
            qqTokenConfigured: true,
            telegramTokenConfigured: false,
            running: true,
            channels: [],
          };
        },
        async listBindings() {
          return [];
        },
      },
      persona: {
        async listFull() {
          return [];
        },
      },
      config: {
        async updateSetting(key, value) {
          calls.updateSetting.push({ key, value });
          if (options.updateSettingFails) throw new Error(options.updateSettingFails);
          return 'updated';
        },
      },
    },
    document: {
      getElementById(id) {
        const map = options.elements || {};
        return Object.prototype.hasOwnProperty.call(map, id) ? map[id] : null;
      },
    },
  });

  if (!view || typeof view.getChannelFields !== 'function') {
    throw new Error('设置页未暴露通道面板接口');
  }
  return { view, calls };
}

// =========================================================
// 字段元数据
// =========================================================

test('通道面板字段与高级页 channel 组逐键一致（默认值防漂移）', () => {
  const { view } = loadSettingsView();
  const panelFields = plain(view.getChannelFields());
  const groups = plain(view.getAdvancedGroups());
  const channelGroup = groups.find((g) => g.key === 'channel');
  assert.ok(channelGroup, '高级设置应含 channel 组');

  const panelByKey = {};
  panelFields.forEach((f) => { panelByKey[f.path[0]] = f.def; });
  const advByKey = {};
  channelGroup.fields.forEach((f) => { advByKey[f.path[0]] = f.def; });

  assert.deepEqual(
    Object.keys(panelByKey).sort(),
    Object.keys(advByKey).sort(),
    '面板字段集合应与高级页 channel 组一致'
  );
  for (const key of Object.keys(panelByKey)) {
    assert.equal(panelByKey[key], advByKey[key], key + ' 默认值应一致');
  }

  // 抽查关键默认值（防两处同时漂移的静默失配）
  assert.equal(panelByKey.enabled, false);
  assert.equal(panelByKey.qq_enabled, false);
  assert.equal(panelByKey.qq_ws_url, 'ws://127.0.0.1:3001');
  assert.equal(panelByKey.telegram_enabled, false);
  assert.equal(panelByKey.default_persona_uid, 'rama-0001');
  assert.equal(panelByKey.proactive_enabled, false);
});

// =========================================================
// 纯函数
// =========================================================

test('连接状态展示视图四态 + 未知兜底', () => {
  const { view } = loadSettingsView();
  assert.deepEqual(plain(view.channelStateView('online')), { label: '在线', level: 'ok' });
  assert.deepEqual(plain(view.channelStateView('connecting')), { label: '连接中', level: 'warn' });
  assert.deepEqual(plain(view.channelStateView('error')), { label: '错误', level: 'danger' });
  assert.deepEqual(plain(view.channelStateView('disconnected')), { label: '已断开', level: 'neutral' });
  assert.deepEqual(plain(view.channelStateView('weird')), { label: '未知', level: 'neutral' });
});

test('绑定行视图模型：标签映射与字段归一', () => {
  const { view } = loadSettingsView();
  const rows = plain(view.buildChannelBindingRows([
    {
      channel: 'qq',
      externalRef: 'c2c:10001',
      personaUid: null,
      effectivePersonaUid: 'rama-0001',
      enabled: true,
      allowProactive: false,
    },
    {
      channel: 'telegram',
      externalRef: 'group:-100200',
      personaUid: 'char-9',
      effectivePersonaUid: 'char-9',
      enabled: false,
      allowProactive: true,
    },
  ]));

  assert.equal(rows.length, 2);
  assert.equal(rows[0].channelLabel, 'QQ');
  assert.equal(rows[0].refLabel, '私聊 · 10001');
  assert.equal(rows[0].personaUid, null);
  assert.equal(rows[0].effectivePersonaUid, 'rama-0001');
  assert.equal(rows[0].enabled, true);
  assert.equal(rows[0].allowProactive, false);

  assert.equal(rows[1].channelLabel, 'Telegram');
  assert.equal(rows[1].refLabel, '群聊 · -100200');
  assert.equal(rows[1].personaUid, 'char-9');
  assert.equal(rows[1].enabled, false);
  assert.equal(rows[1].allowProactive, true);
});

test('绑定行视图模型：异常数据跳过与非数组兜底', () => {
  const { view } = loadSettingsView();
  assert.deepEqual(plain(view.buildChannelBindingRows(null)), []);
  assert.deepEqual(plain(view.buildChannelBindingRows([{ channel: 'qq' }, {}, null])), []);
});

test('凭据保留键映射', () => {
  const { view } = loadSettingsView();
  assert.equal(view.channelCredentialKey('qq'), 'channel.qq_access_token');
  assert.equal(view.channelCredentialKey('telegram'), 'channel.telegram_bot_token');
  assert.equal(view.channelCredentialKey('other'), '');
});

test('表单值合并完整配置：不修改入参且补齐缺失组', () => {
  const { view } = loadSettingsView();
  const original = { channel: { enabled: false, qq_enabled: false }, other: 1 };
  const merged = plain(view.applyChannelFormToConfig(original, {
    enabled: true,
    qq_enabled: true,
    qq_ws_url: 'ws://x',
    telegram_enabled: true,
    default_persona_uid: 'rama-0002',
    proactive_enabled: true,
  }));
  assert.equal(merged.channel.enabled, true);
  assert.equal(merged.channel.qq_enabled, true);
  assert.equal(merged.channel.qq_ws_url, 'ws://x');
  assert.equal(merged.channel.default_persona_uid, 'rama-0002');
  assert.equal(merged.other, 1);
  assert.equal(original.channel.enabled, false, '入参不应被修改');

  const created = plain(view.applyChannelFormToConfig({}, { enabled: true }));
  assert.equal(created.channel.enabled, true);
  assert.equal(created.channel.qq_enabled, undefined, '未传值的键不应写入');
});

// =========================================================
// 凭据写入
// =========================================================

test('凭据写入：经 updateSetting 保留键提交', async () => {
  const { view, calls } = loadSettingsView();
  await view.saveChannelCredential('qq', 'tok-1');
  assert.deepEqual(calls.updateSetting, [{ key: 'channel.qq_access_token', value: 'tok-1' }]);

  let unknownErr = null;
  try {
    await view.saveChannelCredential('other', 'x');
  } catch (e) {
    unknownErr = e;
  }
  assert.ok(unknownErr, '未知平台应抛错');
  assert.match(String(unknownErr.message), /未知平台/);

  let emptyErr = null;
  try {
    await view.saveChannelCredential('qq', '');
  } catch (e) {
    emptyErr = e;
  }
  assert.ok(emptyErr, '空凭据应抛错');
  assert.match(String(emptyErr.message), /不能为空/);
});

test('凭据保存处理器：空输入仅提示不写入', async () => {
  const input = { value: '' };
  const { view, calls } = loadSettingsView({
    elements: { 'settings-channel-qq-token': input },
  });
  await view.handleSaveChannelCredential('qq');
  assert.equal(calls.updateSetting.length, 0, '空输入不应触达写入命令');
  assert.ok(calls.toasts.some((t) => t.type === 'warning'), '应提示警告');
});

test('凭据保存处理器：成功写入后清空输入框', async () => {
  const input = { value: 'tok-2' };
  const { view, calls } = loadSettingsView({
    elements: { 'settings-channel-qq-token': input },
  });
  await view.handleSaveChannelCredential('qq');
  assert.deepEqual(calls.updateSetting, [{ key: 'channel.qq_access_token', value: 'tok-2' }]);
  assert.equal(input.value, '', '保存成功后输入框应清空');
  assert.ok(calls.toasts.some((t) => t.type === 'success'), '应提示成功');
});
