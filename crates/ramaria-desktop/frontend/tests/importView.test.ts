/**
 * tests/importView.test.ts — 导入视图挂载与关键交互
 *
 * parity 清单见 `tests/parity/checklist.ts`（内容与操作在本文件断言，五态逐格覆盖）。
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { describe, expect, it, afterEach } from 'vitest'
import { createMemoryHistory, createRouter, type Router } from 'vue-router'

import type { ImportAnalysisReport, ImportProgressPayload, ImportResult, ImportStatusView } from '../src/core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../src/core/transport'
import { usePersonaStore } from '../src/stores/persona'
import ImportView from '../src/views/import/index.vue'

const REPORT: ImportAnalysisReport = {
  file_path: 'C:/exports/chat.json',
  self_id: 'u_self',
  self_name: '小明',
  self_uin: '10001',
  chat_name: '摄影群',
  chat_type: 'group',
  members: [
    { uid: 'u_a', uin: null, name: '小珊', message_count: 1204 },
    { uid: 'u_b', uin: null, name: '林霁', message_count: 88 },
  ],
  other_name: '',
  other_uid: '',
  other_uin: null,
  time_range: '2024-01-01 ~ 2024-03-01',
  total_raw: 1400,
  total_success: 1390,
  total_degraded: 4,
  total_skipped: 6,
  session_count: 18,
  gap_minutes: 10,
}

const RESULT: ImportResult = {
  success: true,
  mode: 'fast',
  report_summary: '解析 1400 条，写入 1390 条',
  sessions_written: 18,
  messages_written: 1390,
  persona_uid: 'user-0001',
  persona_name: '小明',
  other_persona_uid: 'char-0001',
  other_persona_name: '小珊',
  self_name: '小明',
  chat_name: '摄影群',
  chat_type: 'group',
  members: REPORT.members,
  time_range: REPORT.time_range,
  skipped_count: 6,
  session_ids: ['s-1', 's-2'],
}

const STATUS_IDLE: ImportStatusView = {
  post_active: false,
  pending_attachments: 0,
  skipped_attachments: 0,
  missing_l1_sessions: 0,
  resumable: false,
}

const STATUS_RESUMABLE: ImportStatusView = {
  post_active: false,
  pending_attachments: 2,
  skipped_attachments: 1,
  missing_l1_sessions: 3,
  resumable: true,
}

interface RuntimeConfig {
  status?: ImportStatusView
  report?: ImportAnalysisReport
  result?: ImportResult
  dir?: string | null
  failOnce?: string[]
  holdAnalyze?: boolean
}

interface RecordedCall {
  command: string
  args: Record<string, unknown>
}

type Listener = (event: { payload: unknown }) => void

interface Harness {
  wrapper: VueWrapper
  router: Router
  pinia: Pinia
  calls: RecordedCall[]
  emit: (name: string, payload: unknown) => void
  release: () => void
}

function makeRuntime(config: RuntimeConfig = {}): {
  runtime: TauriRuntime
  calls: RecordedCall[]
  emit: (name: string, payload: unknown) => void
  release: () => void
} {
  const calls: RecordedCall[] = []
  const remaining = new Set(config.failOnce ?? [])
  const listeners = new Map<string, Listener>()
  let release = (): void => {}

  const gate = config.holdAnalyze
    ? new Promise<void>((resolve) => {
        release = resolve
      })
    : Promise.resolve()

  const runtime: TauriRuntime = {
    core: {
      invoke: async (command, args = {}) => {
        calls.push({ command, args })
        if (remaining.has(command)) {
          remaining.delete(command)
          throw new Error('文件格式不是 QQ 聊天记录')
        }
        switch (command) {
          case 'get_import_status':
            return config.status ?? STATUS_IDLE
          case 'open_file_dialog':
            return 'C:/exports/chat.json'
          case 'open_directory_dialog':
            return config.dir === undefined ? 'D:/export' : config.dir
          case 'analyze_qq_chat':
            await gate
            return config.report ?? REPORT
          case 'import_qq_chat':
            return config.result ?? RESULT
          case 'continue_import':
            return null
          default:
            throw new Error(`未预期的命令: ${command}`)
        }
      },
    },
    event: {
      listen: async (name, handler) => {
        listeners.set(name, handler as Listener)
        return () => listeners.delete(name)
      },
      emit: async () => {},
    },
  }

  return {
    runtime,
    calls,
    emit: (name, payload) => {
      listeners.get(name)?.({ payload })
    },
    release: () => release(),
  }
}

async function mountImport(config: RuntimeConfig = {}): Promise<Harness> {
  const { runtime, calls, emit, release } = makeRuntime(config)
  setTauriRuntime(runtime)
  const pinia = createPinia()
  setActivePinia(pinia)
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: { template: '<div />' } },
      { path: '/import', name: 'import', component: { template: '<div />' } },
      { path: '/memory', name: 'memory', component: { template: '<div />' } },
    ],
  })
  await router.push('/import')
  await router.isReady()
  const wrapper = mount(ImportView, {
    global: { plugins: [pinia, router] },
    attachTo: document.body,
  })
  await flushPromises()
  return { wrapper, router, pinia, calls, emit, release }
}

function viewState(wrapper: VueWrapper): string | undefined {
  return wrapper.find('[data-view-state]').attributes('data-view-state')
}

function button(wrapper: VueWrapper, label: string) {
  const hit = wrapper.findAll('button').find((item) => item.text() === label)
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

function lastArgs(calls: RecordedCall[], command: string): Record<string, unknown> | undefined {
  return calls.filter((call) => call.command === command).at(-1)?.args
}

/** 浮层经 Teleport 挂到 body，按文案取按钮 */
function domButton(label: string): HTMLButtonElement {
  const hit = Array.from(document.body.querySelectorAll('button')).find(
    (item) => (item.textContent ?? '').trim() === label,
  )
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

const DONE_PAYLOAD: ImportProgressPayload = {
  phase: 'done',
  current: 17,
  total: 18,
  message: '导入完成：17 个会话生成摘要',
  l1_success: 16,
  l1_failed: 1,
  l2_triggered: false,
  l3_triggered: false,
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('导入选择步', () => {
  it('未选文件时同屏展示文件行占位与模式卡，并查询续跑状态', async () => {
    const { wrapper, calls } = await mountImport()
    expect(viewState(wrapper)).toBe('empty')
    expect(wrapper.text()).toContain('未选择文件')
    expect(wrapper.text()).toContain('快速导入')
    expect(wrapper.text()).toContain('深度导入')
    expect(calls.filter((call) => call.command === 'get_import_status')).toHaveLength(1)
    expect(button(wrapper, '选择文件')).toBeTruthy()
    expect(button(wrapper, '分析文件').attributes('disabled')).toBeDefined()
  })

  it('选择文件后展示文件名与路径、模式卡与双方字段', async () => {
    const { wrapper, calls } = await mountImport()
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()

    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('chat.json')
    expect(wrapper.text()).toContain('C:/exports/chat.json')
    expect(button(wrapper, '分析文件').attributes('disabled')).toBeUndefined()
    expect(button(wrapper, '更换文件')).toBeTruthy()
    expect(wrapper.findAll('button').some((item) => item.text() === '重选文件')).toBe(false)
    expect(wrapper.text()).toContain('我方 Persona 名称')
    expect(wrapper.text()).toContain('对方 Persona UID')

    // 更换文件复用同一对话框，只替换路径，不回到空行
    await button(wrapper, '更换文件').trigger('click')
    await flushPromises()
    expect(calls.filter((call) => call.command === 'open_file_dialog')).toHaveLength(2)
    expect(wrapper.text()).toContain('C:/exports/chat.json')
  })

  it('存在待办时展示继续处理入口与规模', async () => {
    const { wrapper } = await mountImport({ status: STATUS_RESUMABLE })
    expect(wrapper.text()).toContain('检测到可继续处理的事项')
    expect(wrapper.text()).toContain('2 个附件待处理')
    expect(wrapper.text()).toContain('3 个会话缺摘要')
    expect(button(wrapper, '继续处理')).toBeTruthy()
  })
})

describe('预览与执行', () => {
  it('分析后展示统计、群聊成员与确认按钮', async () => {
    const { wrapper, calls } = await mountImport()
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()

    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('文件解析报告')
    expect(wrapper.text()).toContain('成功解析')
    expect(wrapper.text()).toContain('1,390')
    expect(wrapper.text()).toContain('群聊成员')
    expect(wrapper.text()).toContain('小珊（1,204 条）')
    expect(wrapper.text()).toContain('快速导入')

    // 导入模式与对话类型都在信息列中，页头不再单独标注
    const infoLabels = wrapper.findAll('.detail-row dt').map((item) => item.text())
    expect(infoLabels).toContain('导入模式')
    expect(infoLabels).toContain('对话类型')
    expect(wrapper.findAll('.detail-row').find((row) => row.text().includes('对话类型'))?.text()).toContain('群聊')
    expect(lastArgs(calls, 'analyze_qq_chat')).toMatchObject({ gapMinutes: 10 })

    // 动作在右下：重新选择在左、确认导入在右
    const labels = wrapper.findAll('button').map((item) => item.text())
    expect(labels.indexOf('重新选择')).toBeLessThan(labels.indexOf('确认导入'))
  })

  it('确认导入后进入执行步，提交参数带报告回填的名称', async () => {
    const { wrapper, calls } = await mountImport()
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()
    await button(wrapper, '确认导入').trigger('click')
    await flushPromises()

    expect(viewState(wrapper)).toBe('loading')
    expect(wrapper.find('[data-restricted]').attributes('data-restricted')).toBe('locked')
    expect(wrapper.text()).toContain('离开此页不会中断处理')

    expect(lastArgs(calls, 'import_qq_chat')).toMatchObject({
      filePath: 'C:/exports/chat.json',
      mode: 'fast',
      side: 'both',
      gapMinutes: 10,
      personaName: '小明',
      // 群聊没有单一「对方」，报告里的 other_name 为空，提交时保持 null
      otherPersonaName: null,
    })
  })

  it('完成事件后展示统计、L1 失败提示与四个动作', async () => {
    const { wrapper, emit } = await mountImport()
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()
    await button(wrapper, '确认导入').trigger('click')
    await flushPromises()

    emit('import-progress', DONE_PAYLOAD)
    await flushPromises()

    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.find('[data-degraded]').attributes('data-degraded')).toBe('llm')
    expect(wrapper.text()).toContain('导入完成')
    expect(wrapper.text()).toContain('L1 摘要生成失败 1 / 17')
    expect(wrapper.text()).toContain('快速导入未生成记忆摘要')
    expect(wrapper.text()).toContain('写入消息')
    expect(button(wrapper, '前往记忆页面')).toBeTruthy()
    expect(button(wrapper, '查看导入消息')).toBeTruthy()
    expect(button(wrapper, '继续处理')).toBeTruthy()
    expect(button(wrapper, '导入另一个文件')).toBeTruthy()
  })

  it('解析期间进加载态，给出骨架与忙碌标记', async () => {
    const { wrapper, release } = await mountImport({ holdAnalyze: true })
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()

    expect(viewState(wrapper)).toBe('loading')
    expect(wrapper.text()).toContain('正在解析文件，暂不写入数据库…')
    expect(wrapper.findAll('.skeleton').length).toBeGreaterThan(0)
    expect(wrapper.find('[data-view-state]').attributes('aria-busy')).toBe('true')

    release()
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('文件解析报告')
    expect(wrapper.find('[data-view-state]').attributes('aria-busy')).toBeUndefined()
  })

  it('导入执行失败进入错误态并给出重试与更换文件', async () => {
    const { wrapper } = await mountImport({ failOnce: ['import_qq_chat'] })
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()
    await button(wrapper, '确认导入').trigger('click')
    await flushPromises()

    expect(viewState(wrapper)).toBe('error')
    expect(wrapper.find('[data-view-state]').attributes('data-restricted')).toBeUndefined()
    expect(wrapper.text()).toContain('已中断')
    expect(button(wrapper, '重试')).toBeTruthy()
    expect(button(wrapper, '更换文件')).toBeTruthy()
  })

  it('解析失败进入错误态，重试成功回到预览', async () => {
    const { wrapper } = await mountImport({ failOnce: ['analyze_qq_chat'] })
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()

    expect(viewState(wrapper)).toBe('error')
    expect(wrapper.text()).toContain('文件解析失败')
    expect(wrapper.text()).toContain('文件格式不是 QQ 聊天记录')

    await button(wrapper, '重试解析').trigger('click')
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).toContain('文件解析报告')
  })
})

describe('续跑与完成动作', () => {
  it('继续处理选择目录后调用续跑，完成页展示续跑统计', async () => {
    const { wrapper, calls, emit } = await mountImport({ status: STATUS_RESUMABLE })
    await button(wrapper, '继续处理').trigger('click')
    await flushPromises()

    expect(lastArgs(calls, 'open_directory_dialog')).toBeTruthy()
    expect(lastArgs(calls, 'continue_import')).toMatchObject({ exportRoot: 'D:/export' })
    expect(viewState(wrapper)).toBe('loading')

    emit('import-progress', {
      phase: 'done',
      current: 0,
      total: 0,
      message: '继续处理完成',
      reset_count: 3,
      vision_done: 2,
      vision_failed: 1,
      l1_success: 5,
      l1_failed: 0,
    })
    await flushPromises()

    expect(wrapper.text()).toContain('继续处理完成')
    expect(wrapper.text()).toContain('重置待处理附件')
    expect(wrapper.text()).toContain('有 1 张图片未完成理解')
  })

  it('取消目录选择后可选择仅补摘要与画像', async () => {
    const { wrapper, calls } = await mountImport({ status: STATUS_RESUMABLE, dir: null })
    await button(wrapper, '继续处理').trigger('click')
    await flushPromises()

    expect(document.body.textContent).toContain('未选择导出目录')
    domButton('仅补摘要与画像').click()
    await flushPromises()

    expect(lastArgs(calls, 'continue_import')).toMatchObject({ exportRoot: null })
    expect(viewState(wrapper)).toBe('loading')
  })

  it('完成页「前往记忆页面」预选导出者人格', async () => {
    const { wrapper, router, pinia, emit } = await mountImport()
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()
    await button(wrapper, '确认导入').trigger('click')
    await flushPromises()
    emit('import-progress', DONE_PAYLOAD)
    await flushPromises()

    await button(wrapper, '前往记忆页面').trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('memory')
    const personaStore = usePersonaStore(pinia)
    expect(personaStore.consumePreselect()).toBe('user-0001')
  })

  it('完成页「查看导入消息」打开最后一个导入会话', async () => {
    const { wrapper, router, emit } = await mountImport()
    await button(wrapper, '选择文件').trigger('click')
    await flushPromises()
    await button(wrapper, '分析文件').trigger('click')
    await flushPromises()
    await button(wrapper, '确认导入').trigger('click')
    await flushPromises()
    emit('import-progress', DONE_PAYLOAD)
    await flushPromises()

    await button(wrapper, '查看导入消息').trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('chat')
    expect(router.currentRoute.value.query.sessionId).toBe('s-2')
  })
})
