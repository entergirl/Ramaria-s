/**
 * tests/chatComposer.test.ts — 输入区交互
 *
 * 覆盖:
 * - Enter 发送并清空，Shift+Enter 换行，输入法合成期不误发
 * - 空白内容不发送且发送键禁用
 * - 生成中锁定输入与两个按键，占位切换为「正在生成…」
 * - 发图键触发选择，禁用时不响应
 */

import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import ChatComposer from '../src/views/chat/ChatComposer.vue'

function mountComposer(props: Record<string, unknown> = {}) {
  return mount(ChatComposer, {
    props: { modelValue: '你好', ...props },
  })
}

describe('输入区', () => {
  it('Enter 发送并清空输入', async () => {
    const wrapper = mountComposer()
    await wrapper.find('textarea').trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('send')?.[0]).toEqual(['你好'])
    expect(wrapper.emitted('update:modelValue')?.[0]).toEqual([''])
    wrapper.unmount()
  })

  it('Shift+Enter 换行不发送', async () => {
    const wrapper = mountComposer()
    await wrapper.find('textarea').trigger('keydown', { key: 'Enter', shiftKey: true })
    expect(wrapper.emitted('send')).toBeUndefined()
    wrapper.unmount()
  })

  it('输入法合成期的回车不发送，合成结束后可发送', async () => {
    const wrapper = mountComposer()
    const textarea = wrapper.find('textarea')
    await textarea.trigger('compositionstart')
    await textarea.trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('send')).toBeUndefined()
    await textarea.trigger('compositionend')
    await textarea.trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('send')).toHaveLength(1)
    wrapper.unmount()
  })

  it('空白内容不发送，发送键禁用', async () => {
    const wrapper = mountComposer({ modelValue: '   ' })
    expect(wrapper.find('.btn--primary').attributes('disabled')).toBeDefined()
    await wrapper.find('textarea').trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('send')).toBeUndefined()
    wrapper.unmount()
  })

  it('生成中锁定输入与两个按键，占位切换', async () => {
    const wrapper = mountComposer({ locked: true })
    const textarea = wrapper.find('textarea')
    expect(textarea.attributes('disabled')).toBeDefined()
    expect(textarea.attributes('placeholder')).toBe('正在生成…')
    expect(wrapper.find('.attach-btn').attributes('disabled')).toBeDefined()
    expect(wrapper.find('.btn--primary').attributes('disabled')).toBeDefined()
    await wrapper.find('textarea').trigger('keydown', { key: 'Enter' })
    expect(wrapper.emitted('send')).toBeUndefined()
    wrapper.unmount()
  })

  it('不可输入与发图选择中各自给出占位与禁用', async () => {
    const disabled = mountComposer({ disabled: true })
    expect(disabled.find('textarea').attributes('placeholder')).toBe('当前不可输入')
    disabled.unmount()

    const attaching = mountComposer({ attaching: true })
    expect(attaching.find('.attach-btn').attributes('disabled')).toBeDefined()
    await attaching.find('.attach-btn').trigger('click')
    attaching.unmount()
  })

  it('发图键在可用时触发选择', async () => {
    const wrapper = mountComposer()
    await wrapper.find('.attach-btn').trigger('click')
    expect(wrapper.emitted('attach')).toHaveLength(1)
    wrapper.unmount()
  })
})
