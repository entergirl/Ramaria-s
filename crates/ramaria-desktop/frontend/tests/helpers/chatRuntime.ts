/**
 * tests/helpers/chatRuntime.ts — 对话用例的宿主替身
 *
 * 职责:
 * - 提供会话 / 人格 / 详情夹具与可注入的命令、事件替身
 *
 * 设计特点:
 * - 命令未显式给出时按对话视图启动所需的最小集合作答，其余命令报错暴露遗漏
 * - 事件按注册顺序分发，便于用例在流式流程中推进负载
 * - 只替代宿主，不替代业务状态，断言仍读界面与调用记录
 */

import type { SessionDetail, SessionMessage, SessionSummary } from '../../src/core/api/dto'
import { setTauriRuntime } from '../../src/core/transport'

export const NOW = Date.now()

export const RAMA = {
  uid: 'rama-0001',
  name: 'Rama',
  kind: 'rama',
  source: 'manual',
  ref_id: null,
  avatar: null,
  config: null,
  description: null,
  is_active: true,
  created_at: NOW - 100_000,
  updated_at: NOW - 100_000,
}

export const SELF = {
  uid: 'user-0001',
  name: '我',
  kind: 'user',
  source: 'system',
  ref_id: null,
  avatar: null,
  config: null,
  description: null,
  is_active: true,
  created_at: NOW - 100_000,
  updated_at: NOW - 100_000,
}

/** 会话摘要夹具 */
export function summary(patch: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: 's1',
    started_at: NOW - 60_000,
    ended_at: null,
    message_count: 1,
    unread: 0,
    persona_uid: 'rama-0001',
    channel: 'local',
    external_ref: null,
    ...patch,
  }
}

/** 消息夹具 */
export function message(patch: Partial<SessionMessage> = {}): SessionMessage {
  return {
    id: 'm1',
    role: 'user',
    content: '今天有点累',
    persona_uid: 'rama-0001',
    created_at: NOW - 40_000,
    is_proactive: false,
    sender_ref: null,
    sender_name: null,
    recalled_at: null,
    ...patch,
  }
}

/** 会话详情夹具 */
export function detail(channel = 'local', messages: SessionMessage[] = [message()]): SessionDetail {
  return {
    id: 's1',
    started_at: NOW - 60_000,
    ended_at: null,
    persona_uid: 'rama-0001',
    channel,
    total_messages: messages.length,
    has_more: false,
    messages,
  }
}

/** 命令覆盖：键为命令名，值为应答函数 */
export type CommandOverrides = Record<string, (args: Record<string, unknown>) => unknown>

/** 宿主替身 */
export interface ChatRuntime {
  /** 已发生的命令调用 */
  calls: Array<{ command: string; args: Record<string, unknown> }>
  /** 按事件名分发负载 */
  emit: (event: string, payload: unknown) => void
  /** 当前仍生效的事件处理器数 */
  listenerCount: () => number
}

/** 宿主替身选项 */
export interface ChatRuntimeOptions {
  sessions?: SessionSummary[]
  sessionDetail?: SessionDetail
  privacy?: 'NotNeeded' | 'Confirmed' | 'NeedsConfirmation'
  commands?: CommandOverrides
}

/** 安装宿主替身；用例结束需再次以 `null` 清除 */
export function installChatRuntime(options: ChatRuntimeOptions = {}): ChatRuntime {
  const calls: ChatRuntime['calls'] = []
  const listeners = new Map<string, Array<(event: { payload: unknown }) => void>>()
  const overrides = options.commands ?? {}

  setTauriRuntime({
    core: {
      invoke: async (command, args = {}) => {
        calls.push({ command, args })
        const override = overrides[command]
        if (override) {
          return override(args)
        }
        switch (command) {
          case 'list_personas_full':
            return [RAMA, SELF]
          case 'list_sessions':
            return options.sessions ?? [summary()]
          case 'get_session':
            return options.sessionDetail ?? detail()
          case 'check_privacy':
            return { status: options.privacy ?? 'NotNeeded' }
          case 'mark_session_read':
            return 'ok'
          default:
            throw new Error(`未预期的命令: ${command}`)
        }
      },
    },
    event: {
      listen: async <T>(event: string, handler: (incoming: { payload: T }) => void) => {
        const list = listeners.get(event) ?? []
        list.push(handler as (incoming: { payload: unknown }) => void)
        listeners.set(event, list)
        return () => {
          listeners.set(
            event,
            (listeners.get(event) ?? []).filter((item) => item !== handler),
          )
        }
      },
      emit: async () => {},
    },
  })

  return {
    calls,
    emit(event, payload) {
      for (const handler of listeners.get(event) ?? []) {
        handler({ payload })
      }
    },
    listenerCount() {
      let total = 0
      for (const list of listeners.values()) {
        total += list.length
      }
      return total
    },
  }
}

/** 在调用记录里筛出某个命令的参数 */
export function callsOf(runtime: ChatRuntime, command: string): Array<Record<string, unknown>> {
  return runtime.calls.filter((call) => call.command === command).map((call) => call.args)
}
