/**
 * tests/baseStates.test.ts — 基础组件的语义态契约
 *
 * 覆盖:
 * - 15 个基础组件对外暴露的状态属性与无障碍语义（data-* / aria-* / 原生属性）
 * - 展开、选择、关闭、移除等关键交互只断言可观察结果
 *
 * 说明:
 * - 只锁语义与交互，不断言视觉细节；悬停 / 按压由样式伪类承担，不在此处断言
 */

import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it } from 'vitest'

import BaseBubble from '../src/components/base/BaseBubble.vue'
import BaseButton from '../src/components/base/BaseButton.vue'
import BaseCard from '../src/components/base/BaseCard.vue'
import BaseCheckbox from '../src/components/base/BaseCheckbox.vue'
import BaseEmptyState from '../src/components/base/BaseEmptyState.vue'
import BaseInput from '../src/components/base/BaseInput.vue'
import BaseModal from '../src/components/base/BaseModal.vue'
import BaseProgress from '../src/components/base/BaseProgress.vue'
import BaseRadio from '../src/components/base/BaseRadio.vue'
import BaseSegmented from '../src/components/base/BaseSegmented.vue'
import BaseSelect from '../src/components/base/BaseSelect.vue'
import BaseSkeleton from '../src/components/base/BaseSkeleton.vue'
import BaseSwitch from '../src/components/base/BaseSwitch.vue'
import BaseTag from '../src/components/base/BaseTag.vue'
import BaseToast from '../src/components/base/BaseToast.vue'

const OPTIONS = [
  { value: 'local', label: '本地推理' },
  { value: 'online', label: '线上 API' },
]

afterEach(() => {
  document.body.innerHTML = ''
})

describe('Button', () => {
  it('加载态标记加原生禁用，禁用态不标加载', () => {
    const loading = mount(BaseButton, { props: { loading: true } })
    expect(loading.attributes('data-loading')).toBe('')
    expect(loading.attributes('disabled')).toBeDefined()
    expect(loading.find('.btn-spinner').exists()).toBe(true)
    loading.unmount()

    const disabled = mount(BaseButton, { props: { disabled: true } })
    expect(disabled.attributes('data-loading')).toBeUndefined()
    expect(disabled.attributes('disabled')).toBeDefined()
    disabled.unmount()
  })
})

describe('Input', () => {
  it('无效态写入 aria-invalid 并显示错误文案，校验中有转圈', () => {
    const invalid = mount(BaseInput, {
      props: { modelValue: '', label: '名称', error: '名称不能为空', invalid: true },
    })
    expect(invalid.find('input').attributes('aria-invalid')).toBe('true')
    expect(invalid.text()).toContain('名称不能为空')
    invalid.unmount()

    const validating = mount(BaseInput, {
      props: { modelValue: 'x', label: '名称', validating: true, hint: '校验中' },
    })
    expect(validating.attributes('data-validating')).toBe('')
    expect(validating.find('.field-spinner').exists()).toBe(true)
    validating.unmount()
  })
})

describe('Select', () => {
  it('无选项时写「暂无可选项」且点击不展开', async () => {
    const wrapper = mount(BaseSelect, { props: { modelValue: '', label: '服务', options: [] } })
    const trigger = wrapper.find('.select-trigger')
    expect(trigger.text()).toContain('暂无可选项')
    await trigger.trigger('click')
    expect(wrapper.find('.select-list').exists()).toBe(false)
    expect(trigger.attributes('aria-expanded')).toBe('false')
    wrapper.unmount()
  })

  it('键盘上下键展开并用回车选中', async () => {
    const wrapper = mount(BaseSelect, { props: { modelValue: '', label: '服务', options: OPTIONS } })
    const trigger = wrapper.find('.select-trigger')
    await trigger.trigger('keydown', { key: 'ArrowDown' })
    expect(trigger.attributes('aria-expanded')).toBe('true')
    await trigger.trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('update:modelValue')?.[0]).toEqual(['local'])
    wrapper.unmount()
  })

  it('无效态写入 aria-invalid', () => {
    const wrapper = mount(BaseSelect, {
      props: { modelValue: '', label: '服务', options: OPTIONS, error: '请选择服务', invalid: true },
    })
    expect(wrapper.find('.select-trigger').attributes('aria-invalid')).toBe('true')
    wrapper.unmount()
  })
})

describe('Switch', () => {
  it('写入中禁用并给行内指示，开关语义由 role 承担', () => {
    const wrapper = mount(BaseSwitch, {
      props: { modelValue: true, label: '自动保存', saving: true },
    })
    const input = wrapper.find('input')
    expect(input.attributes('role')).toBe('switch')
    expect(input.attributes('disabled')).toBeDefined()
    expect(wrapper.text()).toContain('保存中')
    wrapper.unmount()
  })
})

describe('Checkbox 与 Radio', () => {
  it('勾选态反映原生属性，无效态写入 aria-invalid', () => {
    const checked = mount(BaseCheckbox, { props: { modelValue: true, label: '已知晓' } })
    expect((checked.find('input').element as HTMLInputElement).checked).toBe(true)
    checked.unmount()

    const invalid = mount(BaseCheckbox, {
      props: { modelValue: false, label: '已知晓', invalid: true },
    })
    expect(invalid.find('input').attributes('aria-invalid')).toBe('true')
    invalid.unmount()
  })

  it('半选同步到原生 indeterminate，读屏不再视为未选', async () => {
    const wrapper = mount(BaseCheckbox, {
      props: { modelValue: false, label: '部分选中', indeterminate: true },
    })
    await wrapper.vm.$nextTick()
    expect((wrapper.find('input').element as HTMLInputElement).indeterminate).toBe(true)
    wrapper.unmount()
  })

  it('同一组的单选共享 name，选中项由值决定', () => {
    const wrapper = mount(BaseRadio, {
      props: { modelValue: 'online', value: 'online', name: 'gallery-radio', label: '线上 API' },
    })
    const input = wrapper.find('input')
    expect(input.attributes('name')).toBe('gallery-radio')
    expect((input.element as HTMLInputElement).checked).toBe(true)
    wrapper.unmount()
  })
})

describe('Segmented', () => {
  it('选中段写入 aria-selected，禁用时各段不可点', () => {
    const wrapper = mount(BaseSegmented, {
      props: { modelValue: 'online', options: OPTIONS, label: '服务' },
    })
    expect(wrapper.attributes('role')).toBe('tablist')
    const tabs = wrapper.findAll('[role="tab"]')
    expect(tabs.map((tab) => tab.attributes('aria-selected'))).toEqual(['false', 'true'])
    wrapper.unmount()

    const disabled = mount(BaseSegmented, {
      props: { modelValue: 'online', options: OPTIONS, disabled: true },
    })
    for (const tab of disabled.findAll('[role="tab"]')) {
      expect(tab.attributes('disabled')).toBeDefined()
    }
    disabled.unmount()
  })
})

describe('Tag', () => {
  it('选中态与移除键各自可观察', async () => {
    const wrapper = mount(BaseTag, { props: { selected: true, removable: true } })
    expect(wrapper.attributes('data-selected')).toBe('')
    const remove = wrapper.find('.tag-remove')
    expect(remove.attributes('aria-label')).toBe('移除')
    await remove.trigger('click')
    expect(wrapper.emitted('remove')).toHaveLength(1)
    wrapper.unmount()
  })
})

describe('Card', () => {
  it('可点击卡给出按钮语义并可键盘激活，普通卡不响应', async () => {
    const clickable = mount(BaseCard, { props: { clickable: true } })
    expect(clickable.attributes('role')).toBe('button')
    expect(clickable.attributes('tabindex')).toBe('0')
    await clickable.trigger('keydown', { key: 'Enter' })
    expect(clickable.emitted('activate')).toHaveLength(1)
    clickable.unmount()

    const plain = mount(BaseCard)
    expect(plain.attributes('role')).toBeUndefined()
    expect(plain.attributes('tabindex')).toBeUndefined()
    plain.unmount()
  })

  it('错误与未保存各自带出标记', () => {
    const wrapper = mount(BaseCard, {
      props: { state: 'error', dirty: true, errorText: '读取失败' },
    })
    expect(wrapper.attributes('data-state')).toBe('error')
    expect(wrapper.attributes('data-dirty')).toBe('')
    expect(wrapper.find('.card-error').text()).toContain('读取失败')
    wrapper.unmount()
  })
})

describe('Bubble', () => {
  it('撤回不保留气泡，失败给重试，主动带标识', async () => {
    const recalled = mount(BaseBubble, { props: { role: 'user', variant: 'recalled' } })
    expect(recalled.find('.bubble-recalled').exists()).toBe(true)
    expect(recalled.find('.bubble-body').exists()).toBe(false)
    recalled.unmount()

    const failed = mount(BaseBubble, { props: { role: 'assistant', variant: 'failed' } })
    await failed.find('.bubble-retry').trigger('click')
    expect(failed.emitted('retry')).toHaveLength(1)
    failed.unmount()

    const proactive = mount(BaseBubble, {
      props: { role: 'assistant', variant: 'proactive', name: 'Rama' },
    })
    expect(proactive.text()).toContain('主动')
    proactive.unmount()
  })

  it('群聊形态渲染发言人区', () => {
    const wrapper = mount(BaseBubble, {
      props: { role: 'assistant', name: '小珊', avatar: '珊' },
    })
    expect(wrapper.find('.bubble-avatar').text()).toBe('珊')
    expect(wrapper.find('.bubble-name').text()).toBe('小珊')
    wrapper.unmount()
  })
})

describe('Modal', () => {
  it('打开后为对话框语义，焦点进入框内，Esc 关闭', async () => {
    const wrapper = mount(BaseModal, {
      props: { open: false, title: '撤回这条消息' },
      slots: { default: '<p>正文</p>' },
      attachTo: document.body,
    })
    await wrapper.setProps({ open: true })
    await wrapper.vm.$nextTick()

    const dialog = document.body.querySelector('.modal-card')
    expect(dialog?.getAttribute('role')).toBe('dialog')
    expect(dialog?.getAttribute('aria-modal')).toBe('true')
    expect(dialog?.contains(document.activeElement)).toBe(true)

    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(wrapper.emitted('close')).toHaveLength(1)
    wrapper.unmount()
  })
})

describe('Toast', () => {
  it('类型写在 data-tone，操作与关闭各自派发', async () => {
    const wrapper = mount(BaseToast, {
      props: { tone: 'danger', message: '索引构建失败', actionLabel: '重试' },
    })
    expect(wrapper.attributes('data-tone')).toBe('danger')
    await wrapper.find('.toast-action').trigger('click')
    expect(wrapper.emitted('activate')).toHaveLength(1)
    await wrapper.find('.toast-close').trigger('click')
    expect(wrapper.emitted('close')).toHaveLength(1)
    wrapper.unmount()
  })
})

describe('Progress', () => {
  it('确定进度带数值，不定进度给可读进度条，终态写在 data-state', () => {
    const determinate = mount(BaseProgress, { props: { value: 42, label: '索引构建' } })
    expect(determinate.find('progress').attributes('value')).toBe('42')
    expect(determinate.text()).toContain('42%')
    determinate.unmount()

    const indeterminate = mount(BaseProgress, { props: { label: '处理中' } })
    expect(indeterminate.find('[role="progressbar"]').exists()).toBe(true)
    indeterminate.unmount()

    const failed = mount(BaseProgress, {
      props: { value: 68, label: '构建失败', state: 'failed' },
    })
    expect(failed.attributes('data-state')).toBe('failed')
    failed.unmount()
  })
})

describe('Skeleton 与 EmptyState', () => {
  it('骨架形状与动效各自可观察', () => {
    const card = mount(BaseSkeleton, { props: { shape: 'card', lines: 2 } })
    expect(card.attributes('data-shape')).toBe('card')
    expect(card.attributes('data-motion')).toBe('shimmer')
    card.unmount()

    const still = mount(BaseSkeleton, { props: { shape: 'text', motion: 'static' } })
    expect(still.attributes('data-motion')).toBe('static')
    still.unmount()
  })

  it('空态三变体带归因，操作经插槽给', () => {
    const wrapper = mount(BaseEmptyState, {
      props: { variant: 'blocked', title: '能力不可用', description: '去设置里补齐模型。' },
      slots: { default: '<button type="button">去设置</button>' },
    })
    expect(wrapper.attributes('data-variant')).toBe('blocked')
    expect(wrapper.text()).toContain('能力不可用')
    expect(wrapper.find('.empty-action button').text()).toBe('去设置')
    wrapper.unmount()
  })
})
