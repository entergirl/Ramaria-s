/**
 * tests/statesGallery.test.ts — 状态画廊挂载与工具条
 *
 * 覆盖:
 * - 组件与视图格位全部渲染，每格带验收编号与状态名
 * - 主题切换写入文档根节点，动效切换落到画廊根节点
 * - 视图格位标注里程碑归属
 * - 查询参数可固定深色与减弱动效，便于截图取证
 */

import { mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, describe, expect, it } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

import StatesGallery from '../src/views/dev/StatesGallery.vue'
import { COMPONENT_CELL_COUNT, VIEW_CELL_COUNT } from '../src/views/dev/gallery/catalog'

afterEach(() => {
  document.documentElement.removeAttribute('data-theme')
  document.documentElement.removeAttribute('data-theme-ready')
  document.body.innerHTML = ''
})

async function mountGallery(path = '/dev/states'): Promise<VueWrapper> {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/dev/states', name: 'dev-states', component: { template: '<div />' } }],
  })
  await router.push(path)
  await router.isReady()
  return mount(StatesGallery, {
    global: { plugins: [router] },
    attachTo: document.body,
  })
}

function segmentByText(wrapper: VueWrapper, text: string) {
  return wrapper.findAll('.segmented-item').find((item) => item.text() === text)
}

describe('状态画廊', () => {
  it('渲染全部组件格与视图格，每格带编号与状态名', async () => {
    const wrapper = await mountGallery()
    const cells = wrapper.findAll('[data-cell]')
    expect(cells).toHaveLength(COMPONENT_CELL_COUNT + VIEW_CELL_COUNT)
    for (const cell of cells) {
      expect(cell.attributes('data-qa')).toMatch(/^QA-(C|V)-/)
      expect(cell.text().length).toBeGreaterThan(0)
    }
    expect(wrapper.findAll('[data-row]')).toHaveLength(15 + 9)
    wrapper.unmount()
  })

  it('主题切换写入文档根节点', async () => {
    const wrapper = await mountGallery()
    await segmentByText(wrapper, '深色')?.trigger('click')
    expect(document.documentElement.dataset.theme).toBe('dark')
    await segmentByText(wrapper, '浅色')?.trigger('click')
    expect(document.documentElement.dataset.theme).toBe('light')
    wrapper.unmount()
  })

  it('动效切换落到画廊根节点，默认不标注', async () => {
    const wrapper = await mountGallery()
    expect(wrapper.find('.gallery').attributes('data-motion')).toBeUndefined()
    await segmentByText(wrapper, '减弱动效')?.trigger('click')
    expect(wrapper.find('.gallery').attributes('data-motion')).toBe('reduced')
    wrapper.unmount()
  })

  it('视图行标注里程碑归属', async () => {
    const wrapper = await mountGallery()
    expect(wrapper.find('[data-row="chat"]').text()).toContain('M5')
    expect(wrapper.find('[data-row="settings"]').text()).toContain('M7')
    wrapper.unmount()
  })

  it('查询参数可固定深色与减弱动效', async () => {
    const wrapper = await mountGallery('/dev/states?theme=dark&motion=reduced')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(wrapper.find('.gallery').attributes('data-motion')).toBe('reduced')
    wrapper.unmount()
  })
})
