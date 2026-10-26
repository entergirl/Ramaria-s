/**
 * tests/navDock.test.ts — 圆键导航的入口可见性与位置预设
 *
 * 覆盖:
 * - 全屏页（setup / error）只渲染主题与品牌两键，不列视图入口与设置键
 * - 主界面仍渲染视图入口与各键文字标签
 * - 核心键标识为 🪸，右半屏预设把标签镜像到左侧
 * - 设置页改预设后圆键即时回正；关于浮层只留版本 / 更新 / 调试
 */

import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { nextTick } from 'vue'
import { createMemoryHistory, createRouter } from 'vue-router'
import { beforeEach, describe, expect, it } from 'vitest'

import NavDock from '../src/components/nav/NavDock.vue'
import { useNavStore } from '../src/stores/nav'

const Blank = { template: '<div />' }

async function mountAt(path: string) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: Blank },
      { path: '/memory', name: 'memory', component: Blank },
      { path: '/setup', name: 'setup', component: Blank },
      { path: '/error', name: 'error', component: Blank },
      { path: '/settings', name: 'settings', component: Blank },
    ],
  })
  await router.push(path)
  await router.isReady()

  const pinia = createPinia()
  setActivePinia(pinia)
  const wrapper = mount(NavDock, {
    global: { plugins: [pinia, router] },
    attachTo: document.body,
  })
  await wrapper.find('.orb--core').trigger('click')
  return wrapper
}

beforeEach(() => {
  window.localStorage.clear()
})

describe('圆键导航', () => {
  it('全屏配置页展开后只有主题与品牌两键', async () => {
    const wrapper = await mountAt('/setup')
    expect(wrapper.find('[data-orb="fn-theme"]').exists()).toBe(true)
    expect(wrapper.find('[data-orb="orb-brand"]').exists()).toBe(true)
    expect(wrapper.find('[data-orb="fn-settings"]').exists()).toBe(false)
    expect(wrapper.findAll('[data-orb="sub"]')).toHaveLength(0)
    wrapper.unmount()
  })

  it('主界面展开后仍列出视图入口', async () => {
    const wrapper = await mountAt('/')
    expect(wrapper.findAll('[data-orb="sub"]').length).toBeGreaterThan(0)
    expect(wrapper.find('[data-orb="fn-settings"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('核心键标识为 🪸，视图键带文字标签，贴右缘时镜像标签', async () => {
    const wrapper = await mountAt('/')
    expect(wrapper.find('.orb--core').text()).toContain('🪸')
    expect(wrapper.findAll('[data-orb="sub"] .orb-tip').map((node) => node.text())).toContain('记忆')
    expect(wrapper.classes()).not.toContain('is-mirror')
    wrapper.unmount()

    // 贴右缘的自由位置才镜像；底中 / 右侧预设仍把标签放右侧
    window.localStorage.setItem('ramaria.navFreeX', '1020')
    window.localStorage.setItem('ramaria.navFreeY', '400')
    const right = await mountAt('/')
    expect(right.classes()).toContain('is-mirror')
    right.unmount()
  })

  it('设置页改预设后圆键即时回正', async () => {
    const wrapper = await mountAt('/')
    const element = wrapper.element as HTMLElement
    expect(element.style.left).toBe('512px')

    useNavStore().setPreset('top-left')
    await nextTick()
    expect(element.style.left).toBe('132px')
    expect(element.style.top).toBe('132px')
    wrapper.unmount()
  })

  it('点击视图键切到对应视图', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: '/', name: 'chat', component: Blank },
        { path: '/memory', name: 'memory', component: Blank },
      ],
    })
    await router.push('/')
    await router.isReady()
    const pinia = createPinia()
    setActivePinia(pinia)
    const wrapper = mount(NavDock, {
      global: { plugins: [pinia, router] },
      attachTo: document.body,
    })
    await wrapper.find('.orb--core').trigger('click')

    const memory = wrapper
      .findAll('[data-orb="sub"]')
      .find((node) => node.text().includes('记忆'))
    await memory?.trigger('click')
    await flushPromises()

    expect(router.currentRoute.value.name).toBe('memory')
    wrapper.unmount()
  })

  it('关于浮层只留版本 / 更新 / 调试，导航位置不在其中', async () => {
    const wrapper = await mountAt('/')
    await wrapper.find('[data-orb="orb-brand"]').trigger('click')
    await flushPromises()

    const dialog = document.querySelector('[role="dialog"]')
    expect(dialog?.textContent).toContain('检查更新')
    expect(dialog?.textContent).toContain('调试')
    expect(dialog?.textContent).not.toContain('导航位置')
    expect(dialog?.textContent).not.toContain('索引')
    wrapper.unmount()
  })
})
