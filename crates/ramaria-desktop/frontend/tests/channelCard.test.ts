/**
 * tests/channelCard.test.ts — 社交接入卡的凭据清除
 *
 * 覆盖:
 * - 已配置时出现「清除」入口；未配置时不出现
 * - 清除需二次确认；确认后调用 clear_channel_credential 并以返回视图刷新面板
 */

import { flushPromises, mount } from '@vue/test-utils'
import { nextTick } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { ChannelPanel } from '../src/core/api/dto'
import type { SettingGroup } from '../src/config/schema/define'

const { channelApiMock } = vi.hoisted(() => {
  const panel: ChannelPanel = {
    enabled: true,
    qqEnabled: true,
    telegramEnabled: false,
    qqWsUrl: 'ws://127.0.0.1:3001',
    defaultPersonaUid: 'rama-0001',
    proactiveDeliveryEnabled: false,
    qqTokenConfigured: true,
    telegramTokenConfigured: false,
    running: false,
    channels: [],
  }
  return {
    channelApiMock: {
      getStatus: vi.fn(async () => panel),
      clearCredential: vi.fn(async () => ({ ...panel, qqTokenConfigured: false })),
      listBindings: vi.fn(async () => []),
      start: vi.fn(async () => panel),
      stop: vi.fn(async () => panel),
      setBinding: vi.fn(),
    },
  }
})

vi.mock('../src/core/api', () => ({
  channelApi: channelApiMock,
  configApi: { updateSetting: vi.fn(async () => 'ok') },
  personaApi: { listFull: vi.fn(async () => []) },
}))

import ChannelCard from '../src/views/settings/custom/ChannelCard.vue'

const GROUP: SettingGroup = { id: 'channel', title: '社交平台接入', fields: [] }

function buttonTexts(host: ParentNode): string[] {
  return Array.from(host.querySelectorAll('button')).map((item) => (item.textContent ?? '').trim())
}

function findButton(host: ParentNode, text: string): HTMLButtonElement | null {
  return (
    Array.from(host.querySelectorAll('button')).find(
      (item) => (item.textContent ?? '').trim() === text,
    ) ?? null
  )
}

async function mountCard() {
  const wrapper = mount(ChannelCard, {
    props: { group: GROUP, document: {} },
    attachTo: document.body,
  })
  await flushPromises()
  await nextTick()
  return wrapper
}

beforeEach(() => {
  channelApiMock.clearCredential.mockClear()
})

afterEach(() => {
  document.body.innerHTML = ''
})

describe('通道卡凭据清除', () => {
  it('已配置时提供清除入口，确认后调用命令并刷新为未配置', async () => {
    const wrapper = await mountCard()
    const clearButton = wrapper
      .findAll('button')
      .find((item) => item.text() === '清除')
    expect(clearButton).toBeTruthy()

    await clearButton?.trigger('click')
    await flushPromises()

    expect(document.body.textContent).toContain('清除 QQ 凭据？')
    const modal = document.body.querySelector('.modal-card')
    expect(modal).toBeTruthy()
    const confirm = findButton(modal as Element, '清除')
    expect(confirm).toBeTruthy()
    confirm?.click()
    await flushPromises()
    await nextTick()

    expect(channelApiMock.clearCredential).toHaveBeenCalledWith('qq')
    const placeholder = wrapper.find('input[type="password"]').attributes('placeholder')
    expect(placeholder).toBe('未配置')
    expect(wrapper.findAll('button').some((item) => item.text() === '清除')).toBe(false)

    wrapper.unmount()
  })

  it('未配置时不显示清除入口', async () => {
    channelApiMock.getStatus.mockResolvedValueOnce({
      enabled: true,
      qqEnabled: true,
      telegramEnabled: false,
      qqWsUrl: 'ws://127.0.0.1:3001',
      defaultPersonaUid: 'rama-0001',
      proactiveDeliveryEnabled: false,
      qqTokenConfigured: false,
      telegramTokenConfigured: false,
      running: false,
      channels: [],
    })
    const wrapper = await mountCard()
    expect(buttonTexts(wrapper.element.ownerDocument.body)).not.toContain('清除')
    wrapper.unmount()
  })
})
