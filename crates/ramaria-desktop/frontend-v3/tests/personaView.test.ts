/**
 * tests/personaView.test.ts — 人格视图挂载与关键交互
 *
 * 覆盖:
 * - 列表渲染、默认标记、统计行与加载 / 空 / 错误三态的入口
 * - 进入详情、编辑草稿、保存参数形状与脏状态
 * - 未保存返回确认、设为默认、重载画像确认
 * - 提示词两档：静态自动读取、全量需输入
 * - 预设导入两段式与导出（含示例开关）
 */

import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { nextTick } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'

import type {
  PersonaExportReceipt,
  PersonaFull,
  PersonaPromptPreview,
  PresetImportOutcome,
  PresetImportPreview,
  ProfileStatus,
  SettingItem,
} from '../src/core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../src/core/transport'
import { useSessionStore } from '../src/stores/session'
import PersonaView from '../src/views/persona/index.vue'

const NOW = Date.now()

const RAMA: PersonaFull = {
  uid: 'rama-0001',
  name: '小珊',
  kind: 'rama',
  source: 'manual',
  ref_id: null,
  avatar: null,
  config: null,
  description: '温和、略慢热的陪伴型人格，擅长把大目标拆成小步。',
  is_active: true,
  created_at: NOW - 900000,
  updated_at: NOW - 7200000,
}

const SELF: PersonaFull = {
  uid: 'user-0001',
  name: '我',
  kind: 'user',
  source: 'system',
  ref_id: null,
  avatar: null,
  config: null,
  description: '用户自身人格。',
  is_active: true,
  created_at: NOW - 900000,
  updated_at: NOW - 86400000,
}

const CHAR: PersonaFull = {
  uid: 'char-1001',
  name: '林霁',
  kind: 'char',
  source: 'qq',
  ref_id: 'qq-1001',
  avatar: 'avatar.png',
  config: null,
  description: null,
  is_active: true,
  created_at: NOW - 800000,
  updated_at: NOW - 3600000,
}

const STATUS: ProfileStatus = {
  persona_uid: 'rama-0001',
  n_total_eff: 42,
  active_trait_count: 2,
  status: 'preliminary',
  status_text: '画像初步，随对话持续收敛',
}

const PRESET_PREVIEW: PresetImportPreview = {
  text: '预设裁决文本：新建 char-0002，特质 2 / 事实 1 / 规则 0 / 示例 0。',
  uid: 'char-0002',
  mode: 'create',
  name: '雾',
  decompose_enabled: true,
  privacy_confirmed: false,
}

const PRESET_OUTCOME: PresetImportOutcome = {
  text: '写入结果：特质 2 / 事实 1。',
  uid: 'char-0002',
  mode: 'create',
  name: '雾',
  written: { traits: 2, facts: 1, examples: 0, behavior_rules: 0, failed: 0 },
  decompose: { requested: true, executed: true, fact_count: 1, trait_count: 2 },
}

interface RuntimeConfig {
  personas?: PersonaFull[]
  settings?: SettingItem[]
  status?: ProfileStatus
  presetPreview?: PresetImportPreview
  presetOutcome?: PresetImportOutcome
  pickedFile?: string | null
  savePath?: string | null
  failOnce?: string[]
  failAlways?: string[]
  holdList?: boolean
}

interface RecordedCall {
  command: string
  args: Record<string, unknown>
}

function makeRuntime(config: RuntimeConfig = {}): {
  runtime: TauriRuntime
  calls: RecordedCall[]
  release: () => void
} {
  const calls: RecordedCall[] = []
  const remaining = new Set(config.failOnce ?? [])
  const always = new Set(config.failAlways ?? [])
  let release = (): void => {}

  const gate = config.holdList
    ? new Promise<void>((resolve) => {
        release = resolve
      })
    : Promise.resolve()

  const personas = config.personas ?? [RAMA, SELF, CHAR]

  const runtime: TauriRuntime = {
    core: {
      invoke: async (command, args = {}) => {
        calls.push({ command, args })
        if (always.has(command) || remaining.has(command)) {
          remaining.delete(command)
          throw new Error('数据库读取失败')
        }
        switch (command) {
          case 'list_personas_full':
            await gate
            return personas
          case 'get_settings':
            return config.settings ?? [{ key: 'default_persona_uid', value: 'rama-0001' }]
          case 'get_profile_status':
            return config.status ?? STATUS
          case 'update_persona_info': {
            const base = personas.find((item) => item.uid === args.uid) ?? RAMA
            const request = (args.request ?? {}) as Partial<PersonaFull>
            return { ...base, ...request, updated_at: Date.now() }
          }
          case 'update_setting':
            return 'ok'
          case 'refresh_persona':
            return 'ok'
          case 'preview_persona_prompt': {
            const input = typeof args.input === 'string' ? args.input : ''
            const preview: PersonaPromptPreview = {
              uid: String(args.uid),
              tier: input ? 'full' : 'static',
              system_prompt: input ? `含召回：${input}` : '你是小珊。',
              memory_context: input ? '相关记忆：独自探店' : null,
              chars: 5,
              tokens: 3,
            }
            return preview
          }
          case 'preview_persona_import':
            return config.presetPreview ?? PRESET_PREVIEW
          case 'import_persona_preset':
            return config.presetOutcome ?? PRESET_OUTCOME
          case 'open_file_dialog':
            return config.pickedFile === undefined ? 'C:/presets/wu.toml' : config.pickedFile
          case 'save_file_dialog':
            return config.savePath === undefined ? 'C:/tmp/out.toml' : config.savePath
          case 'persona_export': {
            const receipt: PersonaExportReceipt = {
              uid: String(args.uid),
              name: '小珊',
              writtenTo: String(args.outputPath),
              stats: {
                traits: 3,
                facts: 2,
                behaviorRules: 1,
                blocks: 2,
                promptPatch: true,
                examplesIncluded: args.withExamples ? 4 : 0,
              },
            }
            return receipt
          }
          default:
            throw new Error(`未预期的命令: ${command}`)
        }
      },
    },
    event: {
      listen: async () => () => {},
      emit: async () => {},
    },
  }

  return { runtime, calls, release: () => release() }
}

async function mountPersona(config: RuntimeConfig = {}): Promise<{
  wrapper: VueWrapper
  calls: RecordedCall[]
  release: () => void
}> {
  const { runtime, calls, release } = makeRuntime(config)
  setTauriRuntime(runtime)
  const pinia = createPinia()
  setActivePinia(pinia)
  const wrapper = mount(PersonaView, {
    global: { plugins: [pinia] },
    attachTo: document.body,
  })
  await flushPromises()
  return { wrapper, calls, release }
}

function countOf(calls: RecordedCall[], command: string): number {
  return calls.filter((call) => call.command === command).length
}

function lastArgs(calls: RecordedCall[], command: string): Record<string, unknown> | undefined {
  const hits = calls.filter((call) => call.command === command)
  return hits[hits.length - 1]?.args
}

function viewState(wrapper: VueWrapper): string | undefined {
  return wrapper.find('.persona').attributes('data-view-state')
}

function wrapperButton(wrapper: VueWrapper, label: string) {
  const hit = wrapper.findAll('button').find((item) => item.text().trim() === label)
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

function hasButton(wrapper: VueWrapper, label: string): boolean {
  return wrapper.findAll('button').some((item) => item.text().trim() === label)
}

function domButton(label: string): HTMLButtonElement {
  const hit = Array.from(document.body.querySelectorAll('button')).find(
    (item) => (item.textContent ?? '').trim() === label,
  )
  if (!hit) {
    throw new Error(`未找到按钮: ${label}`)
  }
  return hit
}

async function openDetail(wrapper: VueWrapper, index = 0): Promise<void> {
  const cards = wrapper.findAll('.persona-card')
  const card = cards[index]
  if (!card) {
    throw new Error(`未找到第 ${index} 张人格卡`)
  }
  await card.trigger('click')
  await flushPromises()
}

afterEach(() => {
  setTauriRuntime(null)
  document.body.innerHTML = ''
})

describe('人格列表', () => {
  it('渲染人格卡、类型标签与默认统计', async () => {
    const { wrapper, calls } = await mountPersona()

    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.findAll('.persona-card')).toHaveLength(3)
    expect(wrapper.text()).toContain('3 个 · 1 个默认')
    expect(wrapper.text()).toContain('小珊')
    expect(wrapper.text()).toContain('温和、略慢热的陪伴型人格')
    // 类型标签保留原始枚举
    expect(wrapper.text()).toContain('rama')
    expect(wrapper.text()).toContain('user')
    // 默认人格来自 settings
    expect(calls.some((call) => call.command === 'get_settings')).toBe(true)
    expect(wrapper.findAll('.persona-card')[0]?.text()).toContain('默认')
    expect(wrapper.findAll('.persona-card')[2]?.text()).toContain('暂无描述')
    wrapper.unmount()
  })

  it('列表读取中给出骨架与忙碌标记', async () => {
    const { wrapper, release } = await mountPersona({ holdList: true })

    expect(viewState(wrapper)).toBe('loading')
    expect(wrapper.findAll('.sk-card')).toHaveLength(4)
    expect(wrapper.find('.persona').attributes('aria-busy')).toBe('true')

    release()
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    wrapper.unmount()
  })

  it('空列表给出导入入口，点击打开导入弹窗', async () => {
    const { wrapper } = await mountPersona({ personas: [] })

    expect(viewState(wrapper)).toBe('empty')
    expect(wrapper.text()).toContain('还没有人格')
    await wrapperButton(wrapper, '导入预设').trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('导入人格预设')
    expect(document.body.textContent).toContain('尚未选择预设')
    wrapper.unmount()
  })

  it('列表读取失败给出错误态与重试', async () => {
    const { wrapper, calls } = await mountPersona({ failOnce: ['list_personas_full'] })

    expect(viewState(wrapper)).toBe('error')
    expect(wrapper.text()).toContain('人格列表加载失败')
    expect(wrapper.text()).toContain('数据库读取失败')

    await wrapperButton(wrapper, '重试').trigger('click')
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    expect(countOf(calls, 'list_personas_full')).toBe(2)
    wrapper.unmount()
  })
})

describe('人格详情', () => {
  it('进入详情展示基础资料、元数据与画像状态', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper)

    expect(wrapper.text()).toContain('基础资料')
    expect(wrapper.text()).toContain('画像与元数据')
    expect(wrapper.text()).toContain('rama · 本地人格')
    expect(wrapper.text()).toContain('默认人格')
    expect(wrapper.text()).toContain('L3 画像 · 初步')
    expect(wrapper.text()).toContain('样本 42')
    expect(calls.some((call) => call.command === 'get_profile_status')).toBe(true)
    wrapper.unmount()
  })

  it('画像未生成时给出降级提示', async () => {
    const { wrapper } = await mountPersona({
      status: { ...STATUS, active_trait_count: 0, status: 'insufficient' },
    })
    await openDetail(wrapper)

    expect(wrapper.text()).toContain('画像尚未生成')
    expect(wrapper.text()).toContain('重载画像可尝试重算')
    expect(wrapper.find('.status-box').attributes('data-degraded')).toBe('')
    wrapper.unmount()
  })

  it('编辑名称后出现未保存标记，保存按请求形状提交', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper)

    await wrapper.find('#persona-name-input').setValue('新名字')
    expect(wrapper.text()).toContain('未保存')
    const save = wrapperButton(wrapper, '保存修改')
    expect(save.attributes('disabled')).toBeUndefined()

    await save.trigger('click')
    await flushPromises()

    // 头像未改动时不下发，交由后端保持旧值
    expect(lastArgs(calls, 'update_persona_info')).toEqual({
      uid: 'rama-0001',
      request: {
        name: '新名字',
        description: '温和、略慢热的陪伴型人格，擅长把大目标拆成小步。',
      },
    })
    expect(wrapper.text()).not.toContain('未保存')
    expect(wrapper.text()).toContain('新名字')
    wrapper.unmount()
  })

  it('清除头像时下发空串以清除旧值', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper, 2)

    await wrapperButton(wrapper, '清除').trigger('click')
    await wrapperButton(wrapper, '保存修改').trigger('click')
    await flushPromises()

    expect(lastArgs(calls, 'update_persona_info')).toMatchObject({
      uid: 'char-1001',
      request: { avatar: '' },
    })
    wrapper.unmount()
  })

  it('头像只收图片：非图片文件给出提示且不改草稿', async () => {
    const { wrapper } = await mountPersona()
    await openDetail(wrapper)

    const input = wrapper.find('.file-input')
    const element = input.element as HTMLInputElement
    Object.defineProperty(element, 'files', {
      value: [new File(['x'], 'note.txt', { type: 'text/plain' })],
      configurable: true,
    })
    await input.trigger('change')
    await flushPromises()

    expect(wrapper.text()).toContain('请选择 PNG / JPEG / WebP / GIF 图片')
    expect(wrapper.text()).not.toContain('未保存')
    wrapper.unmount()
  })

  it('横幅头像取已保存图片，缺失时回退首字', async () => {
    const { wrapper } = await mountPersona()
    await openDetail(wrapper, 2)

    expect(wrapper.find('.toolbar .avatar img').attributes('src')).toBe('avatar.png')

    await wrapperButton(wrapper, '人格').trigger('click')
    await flushPromises()
    await openDetail(wrapper, 0)
    expect(wrapper.find('.toolbar .avatar img').exists()).toBe(false)
    expect(wrapper.find('.toolbar .avatar').text()).toBe('小')
    wrapper.unmount()
  })

  it('保存后同步对话页的人格缓存（头像与昵称）', async () => {
    const { wrapper } = await mountPersona()
    const session = useSessionStore()
    session.personas = [RAMA]
    await openDetail(wrapper)

    await wrapper.find('#persona-name-input').setValue('新名字')
    await wrapperButton(wrapper, '保存修改').trigger('click')
    await flushPromises()

    expect(session.personas[0]?.name).toBe('新名字')
    wrapper.unmount()
  })

  it('名称为空时禁止保存并提示原因', async () => {
    const { wrapper } = await mountPersona()
    await openDetail(wrapper)

    await wrapper.find('#persona-name-input').setValue('   ')
    expect(wrapper.text()).toContain('名称不能为空')
    expect(wrapperButton(wrapper, '保存修改').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it('未保存返回列表先确认，确认后丢弃草稿', async () => {
    const { wrapper } = await mountPersona()
    await openDetail(wrapper)

    await wrapper.find('#persona-name-input').setValue('改了一半')
    await wrapperButton(wrapper, '人格').trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('放弃未保存的修改？')

    domButton('放弃并返回').click()
    await flushPromises()
    expect(viewState(wrapper)).toBe('default')
    expect(wrapper.text()).not.toContain('基础资料')
    wrapper.unmount()
  })

  it('设为默认写入本地设置并更新标记', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper, 2)

    await wrapperButton(wrapper, '设为默认').trigger('click')
    await flushPromises()

    expect(lastArgs(calls, 'update_setting')).toEqual({
      key: 'default_persona_uid',
      value: 'char-1001',
    })
    expect(wrapper.text()).toContain('当前默认')
    wrapper.unmount()
  })

  it('重载画像经确认后提交后台管线', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper)

    await wrapperButton(wrapper, '重载画像').trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('重载性格画像')

    domButton('开始重载').click()
    await flushPromises()
    expect(lastArgs(calls, 'refresh_persona')).toEqual({ uid: 'rama-0001' })
    wrapper.unmount()
  })

  it('提示词查看器：折叠态即读静态段，展开压上两卡并可切编译预览', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper)

    // 折叠态：进详情就读静态段，直接给出正文与字数
    expect(lastArgs(calls, 'preview_persona_prompt')).toEqual({
      uid: 'rama-0001',
      input: null,
    })
    expect(wrapper.find('.prompt-preview').text()).toContain('你是小珊。')
    expect(wrapper.text()).toContain('模板 · 运行时填充记忆与规则')
    expect(wrapper.text()).toContain('5 字')
    expect(hasButton(wrapper, '复制')).toBe(false)

    // 展开：上两张卡压缩，正文整段可读
    await wrapperButton(wrapper, '查看提示词').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('收起提示词')
    expect(wrapper.find('.prompt-text').text()).toContain('你是小珊。')
    expect(wrapper.find('.profile-card').classes()).toContain('is-compact')
    expect(wrapper.find('.meta-card').classes()).toContain('is-compact')
    expect(wrapperButton(wrapper, '复制').exists()).toBe(true)
    expect(countOf(calls, 'preview_persona_prompt')).toBe(1)

    // 编译预览：需示例输入，空输入不下发
    await wrapperButton(wrapper, '编译预览').trigger('click')
    await flushPromises()
    expect(wrapper.find('#persona-prompt-input').exists()).toBe(true)
    expect(wrapper.text()).toContain('编译预览会按示例输入做一次记忆召回')

    await wrapperButton(wrapper, '生成预览').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('输入一句示例文本后再生成全量预览')
    expect(countOf(calls, 'preview_persona_prompt')).toBe(1)

    await wrapper.find('#persona-prompt-input').setValue('在吗')
    await wrapperButton(wrapper, '生成预览').trigger('click')
    await flushPromises()
    expect(lastArgs(calls, 'preview_persona_prompt')).toEqual({
      uid: 'rama-0001',
      input: '在吗',
    })
    expect(wrapper.find('.prompt-text').text()).toContain('含召回：在吗')
    expect(wrapper.text()).toContain('编译预览 · 含示例召回')

    // 回到模板：档位复位为静态段
    await wrapperButton(wrapper, '查看模板').trigger('click')
    await flushPromises()
    expect(wrapper.find('.prompt-text').text()).toContain('你是小珊。')
    expect(wrapper.text()).toContain('静态段 · 不含记忆召回')
    wrapper.unmount()
  })

  it('提示词读取失败时折叠态给出原因，展开自动重取', async () => {
    const { wrapper, calls } = await mountPersona({ failOnce: ['preview_persona_prompt'] })
    await openDetail(wrapper)

    expect(wrapper.text()).toContain('提示词读取失败')
    await wrapperButton(wrapper, '查看提示词').trigger('click')
    await flushPromises()
    expect(countOf(calls, 'preview_persona_prompt')).toBe(2)
    expect(wrapper.find('.prompt-text').text()).toContain('你是小珊。')
    wrapper.unmount()
  })
})

describe('预设导入与导出', () => {
  it('导入两段式：选文件 → 预览 → 确认写入', async () => {
    const { wrapper, calls } = await mountPersona()

    await wrapperButton(wrapper, '导入预设').trigger('click')
    await flushPromises()

    domButton('选择文件').click()
    await flushPromises()
    expect(lastArgs(calls, 'open_file_dialog')).toMatchObject({
      extensions: ['toml', 'json', 'png'],
    })
    expect(lastArgs(calls, 'preview_persona_import')).toEqual({
      filePath: 'C:/presets/wu.toml',
      into: null,
      noAi: false,
    })
    expect(document.body.textContent).toContain('预设裁决文本')
    expect(document.body.textContent).toContain('隐私未确认')

    domButton('导入人格').click()
    await flushPromises()
    expect(lastArgs(calls, 'import_persona_preset')).toEqual({
      filePath: 'C:/presets/wu.toml',
      into: null,
      noAi: false,
    })
    expect(countOf(calls, 'list_personas_full')).toBe(2)
    expect(document.body.textContent).not.toContain('尚未选择预设')
    wrapper.unmount()
  })

  it('导入取消选择文件不发起预览', async () => {
    const { wrapper, calls } = await mountPersona({ pickedFile: null })
    await wrapperButton(wrapper, '导入预设').trigger('click')
    await flushPromises()

    domButton('选择文件').click()
    await flushPromises()

    expect(countOf(calls, 'open_file_dialog')).toBe(1)
    expect(countOf(calls, 'preview_persona_import')).toBe(0)
    expect(document.body.textContent).toContain('尚未选择预设')
    wrapper.unmount()
  })

  it('导出：示例开关传到命令，取消保存不写文件', async () => {
    const { wrapper, calls } = await mountPersona()
    await openDetail(wrapper)

    await wrapperButton(wrapper, '导出人格').trigger('click')
    await flushPromises()
    const box = document.body.querySelector<HTMLInputElement>('.checkbox-input')
    if (!box) {
      throw new Error('未找到示例开关')
    }
    box.checked = true
    box.dispatchEvent(new Event('change'))
    await nextTick()

    domButton('选择位置并导出').click()
    await flushPromises()

    expect(lastArgs(calls, 'save_file_dialog')).toMatchObject({
      default_name: 'ramaria-persona-rama-0001.toml',
      filter_name: '人格预设',
    })
    expect(lastArgs(calls, 'persona_export')).toEqual({
      uid: 'rama-0001',
      withExamples: true,
      outputPath: 'C:/tmp/out.toml',
    })
    expect(document.body.textContent).not.toContain('选择位置并导出')
    wrapper.unmount()
  })
})
