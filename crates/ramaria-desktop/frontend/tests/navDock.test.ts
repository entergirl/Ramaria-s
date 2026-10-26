/**
 * tests/navDock.test.ts — 圆键导航的入口可见性
 *
 * 覆盖:
 * - 全屏页（setup / error）只渲染主题与品牌两键，不列视图入口与设置键
 * - 主界面仍渲染视图入口
 */

import { mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { createMemoryHistory, createRouter } from 'vue-router'
import { describe, expect, it } from 'vitest'

import NavDock from '../src/components/nav/NavDock.vue'

const Blank = { template: '<div />' }

async function mountAt(path: string) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', name: 'chat', component: Blank },
      { path: '/setup', name: 'setup', component: Blank },
      { path: '/error', name: 'error', component: Blank },
      { path: '/settings', name: 'settings', component: Blank },
    ],
  })
  await router.push(path)
  await router.isReady()

  const wrapper = mount(NavDock, {
    global: { plugins: [createPinia(), router] },
    attachTo: document.body,
  })
  await wrapper.find('.orb--core').trigger('click')
  return wrapper
}

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
})
