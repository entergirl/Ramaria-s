/**
 * src/config/schema/basic.ts — 基础设置（11 卡片）的配置键声明
 *
 * 职责:
 * - 声明基础页 6 张配置卡的全部界面键（隐私 / 投递概览 / 名单 / 数据 / 诊断由视图自绘）
 *
 * 设计特点:
 * - 仅覆盖 `config.toml` 的界面键；文件路径与版本字段不进 UI
 * - `[proactive]` 仅保留总开关与节流；判据 / 时段 / 问候由进阶页承载
 * - 标签与提示沿用设置页既有文案，不写实现细节
 */

import { type SettingGroup, field, num, select, text, toggle, whitelist } from './define'

/** 基础页配置卡（顺序由 `settingsSections` 编排） */
export const BASIC_GROUPS: SettingGroup[] = [
  {
    id: 'backend',
    title: '后端配置',
    description: '对话模型的接入方式；云端 provider 首次使用需确认隐私范围。',
    fields: [
      select(
        ['backend', 'provider'],
        'Provider',
        [
          { value: 'lm_studio', label: '本地模式（LM Studio）' },
          { value: 'deepseek', label: '线上 API（DeepSeek）' },
          { value: 'openai', label: '线上 API（OpenAI）' },
        ],
        '本地模式不联网；切换云端需重新确认隐私范围。',
      ),
      text(['backend', 'model_id'], '模型 ID', 'LM Studio 可留空自动检测。'),
      text(['backend', 'base_url'], 'Base URL'),
      toggle(['backend', 'chat_thinking'], '思考模式', '仅 DeepSeek 生效；结构化提取路径恒关闭。'),
    ],
  },
  {
    id: 'embedding',
    title: '嵌入与图片理解',
    description: '语义检索的嵌入设备与图片理解声明；嵌入缺失时仅保留关键词与图谱通道。',
    fields: [
      field(['embedding', 'device'], '嵌入计算设备', 'select', {
        options: [
          { value: 'auto', label: '自动（有 CUDA 用 GPU）' },
          { value: 'cpu', label: 'CPU' },
          { value: 'cuda', label: 'CUDA GPU' },
        ],
        hint: 'CUDA 不可用时静默回退 CPU。',
        apply: 'restart',
      }),
      toggle(
        ['vision', 'model_supports_vision'],
        '当前模型支持图片理解',
        '关闭时图片理解整体跳过，附件保留占位符。',
      ),
      num(['vision', 'batch_limit'], '单批图片上限', { min: 0, unit: '张', hint: '0 = 不限。' }),
    ],
  },
  {
    id: 'session',
    title: '会话',
    description: '空闲封存节奏与对话历史窗口；空闲时长保存后立即生效。',
    fields: [
      num(['session', 'l1_idle_minutes'], '空闲自动保存', {
        min: 5,
        max: 60,
        unit: '分钟',
        apply: 'hot',
        hint: '到点结束当前会话并生成 L1 摘要。',
      }),
      num(['session', 'idle_check_interval_seconds'], '空闲检测间隔', {
        min: 5,
        unit: '秒',
      }),
      num(['session', 'l2_check_interval_seconds'], 'L2/L3 检查间隔', {
        min: 60,
        unit: '秒',
      }),
      num(['session', 'max_history_messages'], '历史消息上限', { min: 0, unit: '条' }),
      num(['session', 'max_history_chars'], '历史字符预算', { min: 0, unit: '字符' }),
    ],
  },
  {
    id: 'proactive-core',
    title: '主动对话 · 总开关与节流',
    description: '决定是否发起、多久发起一次，以及免打扰与日上限。',
    fields: [
      toggle(['proactive', 'enabled'], '启用主动对话', '关闭后不调度、不投递。'),
      num(['proactive', 'check_interval_seconds'], '调度检查间隔', {
        min: 30,
        unit: '秒',
      }),
      num(['proactive', 'min_idle_hours'], '最小空闲时长', { min: 0, unit: '小时' }),
      num(['proactive', 'daily_limit'], '每人每日上限', { min: 1, unit: '条' }),
      num(['proactive', 'daily_total_limit'], '全部人格每日合计', {
        min: 0,
        unit: '条',
        hint: '0 = 不限。',
      }),
      text(['proactive', 'quiet_hours'], '免打扰时段', 'HH:MM-HH:MM，支持跨零点。'),
      num(['proactive', 'cooldown_hours'], '投递冷却', { min: 0, unit: '小时' }),
    ],
  },
  {
    id: 'channel',
    title: '社交平台接入',
    description: 'QQ 与 Telegram 通道的开关、默认人格与投递口径；平台凭据单独保存。',
    fields: [
      toggle(['channel', 'enabled'], '通道总开关', '关闭时不连接任何平台。'),
      text(['channel', 'default_persona_uid'], '默认本机侧人格', '绑定未覆盖时生效。'),
      toggle(['channel', 'qq_enabled'], 'QQ 通道', '需通道总开关开启。'),
      text(['channel', 'qq_ws_url'], 'QQ 正向 WS 地址', 'NapCat 正向 WebSocket 地址。'),
      toggle(['channel', 'telegram_enabled'], 'Telegram 通道', '需通道总开关开启。'),
      toggle(
        ['channel', 'proactive_delivery_enabled'],
        '社交主动投递',
        '还须绑定级开关放行。',
      ),
    ],
  },
  {
    id: 'mcp',
    title: 'MCP 接入',
    description: '外部 MCP 客户端挂载的记忆服务；被检索到的内容会随对话发送给该客户端使用的模型。',
    fields: [
      toggle(['mcp', 'enabled'], '启用 MCP 工具能力', '关闭时不影响桌面与 CLI。'),
      toggle(['mcp', 'allow_ingest'], '允许对话回流写入', undefined),
      toggle(['mcp', 'allow_seal'], '允许触发封存与摘要', '关闭时只写不封存。'),
      whitelist(
        ['mcp', 'allowed_personas'],
        '可见人格白名单',
        [{ value: '*', label: '全部人格' }],
        '「全部人格」与具体人格互斥；留空等同全部。',
      ),
      toggle(['mcp', 'allow_raw_text'], '允许返回原文块', '原文为最高敏感层。'),
      num(['mcp', 'max_items'], '召回条数上限', { min: 1, unit: '条' }),
      num(['mcp', 'max_chars'], '召回文本预算', { min: 0, unit: '字符' }),
    ],
  },
]
