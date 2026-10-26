<script setup lang="ts">
/**
 * src/components/nav/NavDock.vue — 圆键导航
 *
 * 职责:
 * - 常驻核心圆键，展开为 5~6 个视图键，另有主题 / 设置 / 品牌三个功能键
 * - 支持拖动摆放、双击回正、键盘操作、Esc 与遮罩收起
 * - 位置预设读写 `stores/nav`，设置页改动后即时回正
 *
 * 设计特点:
 * - 几何全部来自 `geometry.ts`，组件只把结果写成 CSS 变量（不写内联 style 属性）
 * - 展开方向与紧凑档按可用空间自动判定，窄窗不越界
 * - 视图键标签在键右侧，悬浮 / 聚焦时键微左移、标签向右弹出；右半屏镜像到左侧
 * - 核心键以 🪸 标识，展开时斜向旋转，交互逻辑与图标形态解耦
 * - 收起态核心键降低存在感，不遮挡内容；展开时给出遮罩承接点击
 * - 图标为常量 SVG 字符串，经 v-html 注入，不含任何用户输入
 */

import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import AboutCard from './AboutCard.vue'
import {
  clampPoint,
  decideSide,
  dragBounds,
  exceedsDragThreshold,
  fnOffset,
  labelSide,
  presetPoint,
  resolvePreset,
  subOffset,
  type Bounds,
  type NavPreset,
  type Point,
  type SideDecision,
} from './geometry'
import { FULLSCREEN_ROUTES } from '../../app/router'
import { PREF_KEYS, readPref, removePref, writePref } from '../../core/prefs'
import { currentTheme, toggleTheme } from '../../core/theme'
import { useDebugStore } from '../../stores/debug'
import { useNavStore } from '../../stores/nav'

interface NavItem {
  id: string
  label: string
  icon: string
  debug?: boolean
}

const NAV: NavItem[] = [
  { id: 'chat', label: '对话', icon: 'chat' },
  { id: 'memory', label: '记忆', icon: 'layers' },
  { id: 'persona', label: '人格', icon: 'user' },
  { id: 'rules', label: '规则', icon: 'shield' },
  { id: 'import', label: '导入', icon: 'download' },
  { id: 'debug', label: '调试', icon: 'terminal', debug: true },
]

const ICONS: Record<string, string> = {
  chat: '<svg viewBox="0 0 24 24"><path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z"/></svg>',
  layers:
    '<svg viewBox="0 0 24 24"><polygon points="12 2 2 7 12 12 22 7 12 2"/><polyline points="2 17 12 22 22 17"/><polyline points="2 12 12 17 22 12"/></svg>',
  user: '<svg viewBox="0 0 24 24"><path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>',
  shield:
    '<svg viewBox="0 0 24 24"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><polyline points="9 11.5 11 13.5 15 9.5"/></svg>',
  download:
    '<svg viewBox="0 0 24 24"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>',
  terminal:
    '<svg viewBox="0 0 24 24"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>',
  sliders:
    '<svg viewBox="0 0 24 24"><line x1="4" y1="21" x2="4" y2="14"/><line x1="4" y1="10" x2="4" y2="3"/><line x1="12" y1="21" x2="12" y2="12"/><line x1="12" y1="8" x2="12" y2="3"/><line x1="20" y1="21" x2="20" y2="16"/><line x1="20" y1="12" x2="20" y2="3"/><line x1="1" y1="14" x2="7" y2="14"/><line x1="9" y1="8" x2="15" y2="8"/><line x1="17" y1="16" x2="23" y2="16"/></svg>',
  sun: '<svg class="icon-sun" viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/></svg>',
  moon: '<svg class="icon-moon" viewBox="0 0 24 24"><path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/></svg>',
}

const router = useRouter()
const route = useRoute()
const debug = useDebugStore()
const nav = useNavStore()

const root = ref<HTMLElement | null>(null)
const coreButton = ref<HTMLButtonElement | null>(null)

const open = ref(false)
const aboutOpen = ref(false)
const dragging = ref(false)
const theme = ref(currentTheme())

const anchor = ref<Point>({ x: 0, y: 0 })
const side = ref<SideDecision>({ flipped: false, compact: false })
const freePosition = ref(false)
/** 标签镜像：右侧放不下时标签改挂左侧 */
const mirrored = ref(false)

let suppressClick = false
let pointerStart: Point = { x: 0, y: 0 }
let anchorStart: Point = { x: 0, y: 0 }
let movedDuringDrag = false
let resizeTimer = 0

const currentView = computed(() => String(route.name ?? 'chat'))
/** 全屏页（首次配置 / 错误）：只保留主题与品牌两键，不列出视图入口 */
const fullscreen = computed(() => FULLSCREEN_ROUTES.has(currentView.value))
const items = computed(() =>
  fullscreen.value ? [] : NAV.filter((item) => !item.debug || debug.unlocked),
)
const flipped = computed(() => side.value.flipped)
const compact = computed(() => side.value.compact)

function viewport(): { width: number; height: number } {
  return { width: window.innerWidth, height: window.innerHeight }
}

function applyAnchor(): void {
  const element = root.value
  if (!element) {
    return
  }
  element.style.left = `${Math.round(anchor.value.x)}px`
  element.style.top = `${Math.round(anchor.value.y)}px`
}

/** 预设锚点：全屏页把「底中」解析到右下，避免核心键压在居中卡片上 */
function presetAnchor(name: NavPreset = nav.preset): Point {
  return presetPoint(resolvePreset(name, fullscreen.value), viewport())
}

function applyOffset(selector: string, point: Point, index?: number): void {
  const element = root.value?.querySelector<HTMLElement>(selector)
  if (!element) {
    return
  }
  element.style.setProperty('--ox', `${point.x}px`)
  element.style.setProperty('--oy', `${point.y}px`)
  if (typeof index === 'number') {
    element.style.setProperty('--i', String(index))
  }
}

function applyLayout(): void {
  const element = root.value
  if (!element) {
    return
  }
  element.style.setProperty('--n', String(items.value.length))
  element.querySelectorAll<HTMLElement>('[data-orb="sub"]').forEach((node, index) => {
    const point = subOffset(index, flipped.value, compact.value)
    node.style.setProperty('--ox', `${point.x}px`)
    node.style.setProperty('--oy', `${point.y}px`)
    node.style.setProperty('--i', String(index))
  })
  if (fullscreen.value) {
    // 全屏页只有两键：主题向上、品牌向下；靠近上缘时整体翻转
    applyOffset('[data-orb="fn-theme"]', fnOffset(flipped.value ? 90 : -90, compact.value))
    applyOffset('[data-orb="orb-brand"]', fnOffset(flipped.value ? -90 : 90, compact.value))
    return
  }
  applyOffset('[data-orb="fn-theme"]', fnOffset(180, compact.value))
  applyOffset('[data-orb="fn-settings"]', fnOffset(0, compact.value))
  applyOffset('[data-orb="orb-brand"]', fnOffset(flipped.value ? -90 : 90, compact.value))
}

function updateSide(): void {
  side.value = decideSide(anchor.value, items.value.length, viewport())
  mirrored.value = labelSide(anchor.value, viewport()) === 'left'
  applyLayout()
}

function setOpen(next: boolean): void {
  open.value = next
  if (next) {
    updateSide()
  }
}

function closeDock(): void {
  setOpen(false)
}

function selectView(id: string): void {
  setOpen(false)
  void router.push({ name: id })
}

function openAbout(): void {
  setOpen(false)
  aboutOpen.value = true
}

function switchTheme(): void {
  theme.value = toggleTheme()
}

/** 回到所选预设位置：清掉自由摆放，按预设重算锚点 */
function anchorToPreset(): void {
  removePref(PREF_KEYS.navFreeX)
  removePref(PREF_KEYS.navFreeY)
  freePosition.value = false
  anchor.value = presetAnchor()
  applyAnchor()
  updateSide()
}

/** 切换预设：写偏好后立即落位（同值时不触发订阅，仍手动回正） */
function applyPreset(name: NavPreset): void {
  nav.setPreset(name)
  anchorToPreset()
}

function onToggleDebug(value: boolean): void {
  debug.setUnlocked(value)
  if (!value && currentView.value === 'debug') {
    void router.push({ name: 'chat' })
  }
  updateSide()
}

function onCoreClick(): void {
  if (suppressClick) {
    suppressClick = false
    return
  }
  updateSide()
  setOpen(!open.value)
}

function onCoreDblClick(): void {
  setOpen(false)
  applyPreset(nav.preset)
}

function onCoreKeydown(event: KeyboardEvent): void {
  // 空格默认滚动页面，需拦下；Enter 与空格的原生 click 由按钮自身派发，不重复处理
  if (event.key === ' ' || event.key === 'Spacebar') {
    event.preventDefault()
  }
}

function onPointerDown(event: PointerEvent): void {
  if (event.button !== 0 && event.pointerType === 'mouse') {
    return
  }
  dragging.value = true
  movedDuringDrag = false
  pointerStart = { x: event.clientX, y: event.clientY }
  anchorStart = { ...anchor.value }
  coreButton.value?.setPointerCapture(event.pointerId)
}

function onPointerMove(event: PointerEvent): void {
  if (!dragging.value) {
    return
  }
  const next: Point = { x: event.clientX, y: event.clientY }
  if (!movedDuringDrag) {
    if (!exceedsDragThreshold(pointerStart, next)) {
      return
    }
    movedDuringDrag = true
    setOpen(false)
  }
  const bounds: Bounds = dragBounds(viewport())
  anchor.value = clampPoint(
    {
      x: anchorStart.x + (next.x - pointerStart.x),
      y: anchorStart.y + (next.y - pointerStart.y),
    },
    bounds,
  )
  applyAnchor()
  updateSide()
}

function onPointerUp(): void {
  if (!dragging.value) {
    return
  }
  dragging.value = false
  if (!movedDuringDrag) {
    return
  }
  suppressClick = true
  freePosition.value = true
  writePref(PREF_KEYS.navFreeX, String(Math.round(anchor.value.x)))
  writePref(PREF_KEYS.navFreeY, String(Math.round(anchor.value.y)))
  updateSide()
}

function visibleButtons(): HTMLButtonElement[] {
  const element = root.value
  if (!element) {
    return []
  }
  return Array.from(element.querySelectorAll<HTMLButtonElement>('[data-orb="sub"]'))
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    if (aboutOpen.value) {
      aboutOpen.value = false
      return
    }
    if (open.value) {
      setOpen(false)
      coreButton.value?.focus({ preventScroll: true })
    }
    return
  }
  if (!open.value) {
    return
  }
  if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) {
    return
  }
  event.preventDefault()
  const buttons = visibleButtons()
  if (buttons.length === 0) {
    return
  }
  const index = buttons.indexOf(document.activeElement as HTMLButtonElement)
  const step = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : -1
  const next = (index + step + buttons.length) % buttons.length
  buttons[next]?.focus({ preventScroll: true })
}

function onResize(): void {
  window.clearTimeout(resizeTimer)
  resizeTimer = window.setTimeout(() => {
    if (freePosition.value) {
      anchor.value = clampPoint(anchor.value, dragBounds(viewport()))
    } else {
      anchor.value = presetAnchor()
    }
    applyAnchor()
    updateSide()
  }, 80)
}

onMounted(() => {
  const savedX = Number(readPref(PREF_KEYS.navFreeX))
  const savedY = Number(readPref(PREF_KEYS.navFreeY))
  if (Number.isFinite(savedX) && Number.isFinite(savedY) && savedX > 0 && savedY > 0) {
    freePosition.value = true
    anchor.value = clampPoint({ x: savedX, y: savedY }, dragBounds(viewport()))
  } else {
    anchor.value = presetAnchor()
  }
  applyAnchor()
  updateSide()
  window.addEventListener('resize', onResize)
  document.addEventListener('keydown', onDocumentKeydown)
})

/* 全屏页与主界面共用一套位置预设：进出全屏时重算锚点（自由摆放不干预） */
watch(fullscreen, () => {
  if (!freePosition.value) {
    anchor.value = presetAnchor()
    applyAnchor()
  }
  updateSide()
})

/* 设置页改预设：圆键即时回正并把自由摆放一并清掉 */
watch(
  () => nav.preset,
  () => anchorToPreset(),
)

onBeforeUnmount(() => {
  window.clearTimeout(resizeTimer)
  window.removeEventListener('resize', onResize)
  document.removeEventListener('keydown', onDocumentKeydown)
})
</script>

<template>
  <div
    ref="root"
    class="dock"
    :class="{
      'is-open': open,
      'is-compact': compact,
      'is-flipped': flipped,
      'is-mirror': mirrored,
    }"
  >
    <div class="veil" :class="{ 'is-open': open }" @click="closeDock" />

    <button
      ref="coreButton"
      type="button"
      class="orb orb--core"
      :class="{ 'is-dragging': dragging }"
      :aria-expanded="open ? 'true' : 'false'"
      aria-label="导航"
      @click="onCoreClick"
      @dblclick="onCoreDblClick"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
      @keydown="onCoreKeydown"
    >
      <span class="core-emoji" aria-hidden="true">🪸</span>
    </button>

    <button
      v-for="item in items"
      :key="item.id"
      type="button"
      class="orb orb--sub"
      :class="{ 'is-current': currentView === item.id }"
      data-orb="sub"
      :aria-label="item.label"
      :aria-current="currentView === item.id ? 'page' : undefined"
      :tabindex="open ? 0 : -1"
      @click="selectView(item.id)"
    >
      <span class="orb-icon" v-html="ICONS[item.icon]" />
      <span class="orb-tip">{{ item.label }}</span>
    </button>

    <button
      type="button"
      class="orb orb--fn"
      data-orb="fn-theme"
      :aria-label="theme === 'dark' ? '切换到浅色' : '切换到深色'"
      :tabindex="open ? 0 : -1"
      @click="switchTheme"
    >
      <span class="orb-icon" v-html="ICONS.sun" />
      <span class="orb-icon" v-html="ICONS.moon" />
      <span class="orb-tip">日间 / 夜间</span>
    </button>

    <button
      v-if="!fullscreen"
      type="button"
      class="orb orb--fn"
      data-orb="fn-settings"
      aria-label="设置"
      :class="{ 'is-current': currentView === 'settings' }"
      :tabindex="open ? 0 : -1"
      @click="selectView('settings')"
    >
      <span class="orb-icon" v-html="ICONS.sliders" />
      <span class="orb-tip">设置</span>
    </button>

    <button
      type="button"
      class="orb orb--brand"
      data-orb="orb-brand"
      aria-label="关于 Ramaria"
      :tabindex="open ? 0 : -1"
      @click="openAbout"
    >
      <span class="brand-mark">珊</span>
      <span class="brand-text">
        <span class="brand-name">珊瑚菌</span>
        <span class="brand-sub">Ramaria</span>
      </span>
    </button>

    <AboutCard
      :open="aboutOpen"
      @close="aboutOpen = false"
      @toggle-debug="onToggleDebug"
    />
  </div>
</template>

<style scoped>
.dock {
  position: fixed;
  left: 0;
  top: 0;
  width: 1px;
  height: 1px;
  z-index: var(--z-nav);
  user-select: none;
  touch-action: none;
}

.veil {
  position: fixed;
  inset: 0;
  /* 层级由 DOM 顺序表达：遮罩在模板首位，圆键在其后绘制，故键始终在遮罩之上 */
  background: var(--overlay-scrim);
  opacity: 0;
  pointer-events: none;
  transition: opacity var(--dur-base) var(--ease-out);
}

.veil.is-open {
  opacity: 1;
  pointer-events: auto;
}

.orb {
  position: absolute;
  left: 0;
  top: 0;
  display: grid;
  place-items: center;
  padding: 0;
  border: none;
  border-radius: var(--radius-full);
  cursor: pointer;
  opacity: 0;
  pointer-events: none;
  transform: translate(-50%, -50%) scale(0.4);
  transition:
    transform var(--dur-slow) var(--ease-pop),
    opacity var(--dur-base) var(--ease-out),
    background-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.dock.is-open .orb--sub,
.dock.is-open .orb--fn,
.dock.is-open .orb--brand {
  opacity: 1;
  pointer-events: auto;
  transform: translate(-50%, -50%) translate(var(--ox, 0), var(--oy, 0)) scale(1);
}

/* 子键：开合按索引错峰，悬停位移与配色即时响应（不被错峰延迟拖住） */
.orb--sub {
  transition:
    transform var(--dur-slow) var(--ease-pop) calc((var(--n, 6) - 1 - var(--i, 0)) * 40ms),
    opacity var(--dur-base) var(--ease-out) calc((var(--n, 6) - 1 - var(--i, 0)) * 40ms),
    translate var(--dur-fast) var(--ease-out) 0ms,
    background-color var(--dur-fast) var(--ease-out) 0ms,
    border-color var(--dur-fast) var(--ease-out) 0ms,
    box-shadow var(--dur-fast) var(--ease-out) 0ms;
}

.dock:not(.is-open) .orb--sub {
  transition:
    transform var(--dur-slow) var(--ease-pop) calc(var(--i, 0) * 26ms),
    opacity var(--dur-base) var(--ease-out) calc(var(--i, 0) * 26ms),
    translate 0ms,
    background-color var(--dur-fast) var(--ease-out) 0ms,
    border-color var(--dur-fast) var(--ease-out) 0ms,
    box-shadow var(--dur-fast) var(--ease-out) 0ms;
}

.orb--core {
  width: 62px;
  height: 62px;
  opacity: 1;
  pointer-events: auto;
  transform: translate(-50%, -50%);
  background: var(--btn-bg);
  color: var(--btn-fg);
  box-shadow:
    var(--btn-shadow),
    0 0 0 6px var(--bg-page);
  cursor: grab;
}

.dock:not(.is-open) .orb--core {
  opacity: 0.58;
}

.dock:not(.is-open) .orb--core:hover,
.dock:not(.is-open) .orb--core:focus-visible {
  opacity: 1;
}

.orb--core:hover {
  transform: translate(-50%, -50%) scale(1.06);
}

.orb--core:active {
  transform: translate(-50%, -50%) scale(0.94);
}

.orb--core.is-dragging {
  cursor: grabbing;
  transform: translate(-50%, -50%) scale(1.08);
}

.orb--core:focus-visible {
  outline: none;
  box-shadow:
    var(--btn-shadow),
    var(--focus-ring);
}

.core-emoji {
  display: block;
  font-family:
    'Segoe UI Emoji', 'Apple Color Emoji', 'Noto Color Emoji', sans-serif;
  font-size: 26px;
  line-height: 1;
  transition: transform var(--dur-base) var(--ease-out);
}

/* 展开时向左倾倒 45°，收起回正；旋转与点击逻辑解耦，只作状态提示 */
.dock.is-open .core-emoji {
  transform: rotate(-45deg);
}

.orb--sub {
  width: 52px;
  height: 52px;
  background: var(--bg-card);
  border: 1px solid var(--border);
  color: var(--brand-text);
  box-shadow: var(--shadow-md);
}

.orb--sub:hover {
  border-color: var(--brand-line);
  background: var(--brand-surface);
}

/* 悬浮 / 聚焦：键微移给标签让位；镜像侧方向相反 */
.dock.is-open .orb--sub:hover,
.dock.is-open .orb--sub:focus-visible {
  translate: calc(var(--space-6) * -1) 0;
}

.dock.is-open.is-mirror .orb--sub:hover,
.dock.is-open.is-mirror .orb--sub:focus-visible {
  translate: var(--space-6) 0;
}

.orb--sub.is-current {
  background: var(--brand-surface);
  border-color: var(--brand);
  color: var(--brand-text);
  font-weight: var(--fw-semibold);
}

.orb--fn {
  width: 48px;
  height: 48px;
  background: var(--bg-card);
  border: 1px solid var(--border);
  color: var(--text-2);
  box-shadow: var(--shadow-md);
}

.orb--fn:hover {
  color: var(--brand-text);
  border-color: var(--brand-line);
}

.orb--fn.is-current {
  border-color: var(--brand);
  color: var(--brand-text);
}

.orb--brand {
  display: inline-flex;
  gap: var(--space-8);
  align-items: center;
  height: 52px;
  padding: 0 var(--space-16) 0 var(--space-8);
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--bg-card);
  color: var(--text-1);
  box-shadow: var(--shadow-lg);
}

.orb--brand:hover {
  border-color: var(--brand-line);
}

.brand-mark {
  display: grid;
  place-items: center;
  flex: none;
  width: 38px;
  height: 38px;
  border-radius: var(--radius-full);
  background: var(--brand);
  color: var(--btn-fg);
  font-size: var(--fs-body-lg);
  font-weight: var(--fw-bold);
}

.brand-text {
  display: flex;
  flex-direction: column;
  line-height: 1.15;
  white-space: nowrap;
}

.brand-name {
  font-size: var(--fs-body);
  font-weight: var(--fw-bold);
}

.brand-sub {
  color: var(--text-3);
  font-family: var(--font-display);
  font-size: var(--fs-caption);
  font-style: italic;
}

.orb-icon {
  display: grid;
  place-items: center;
  /* 主题键的两个图标叠在同一格，隐藏的图标不占位，图标始终居中 */
  grid-area: 1 / 1;
}

.orb-icon :deep(svg) {
  width: 21px;
  height: 21px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.orb--fn .orb-icon :deep(svg) {
  width: 19px;
  height: 19px;
}

.orb-icon :deep(.icon-moon) {
  display: none;
}

:global([data-theme='dark']) .orb-icon :deep(.icon-sun) {
  display: none;
}

:global([data-theme='dark']) .orb-icon :deep(.icon-moon) {
  display: block;
}

.orb-tip {
  position: absolute;
  left: 50%;
  bottom: calc(100% + var(--space-6));
  transform: translateX(-50%);
  padding: var(--space-4) var(--space-8);
  border-radius: var(--radius-tag);
  background: var(--bg-card);
  border: 1px solid var(--border);
  color: var(--text-2);
  font-size: var(--fs-caption);
  white-space: nowrap;
  box-shadow: var(--shadow-xs);
  opacity: 0;
  pointer-events: none;
  transition: opacity var(--dur-fast) var(--ease-out);
}

.orb:hover .orb-tip,
.orb:focus-visible .orb-tip {
  opacity: 1;
}

/* 视图键：标签在键右侧并与键垂直居中，悬浮时向右浮出；右半屏镜像到左侧 */
.orb--sub .orb-tip {
  left: calc(100% + var(--space-10));
  top: 50%;
  bottom: auto;
  transform: translate(calc(var(--space-6) * -1), -50%);
  transition:
    opacity var(--dur-fast) var(--ease-out),
    transform var(--dur-base) var(--ease-out);
}

.dock.is-mirror .orb--sub .orb-tip {
  left: auto;
  right: calc(100% + var(--space-10));
  transform: translate(var(--space-6), -50%);
}

.orb--sub:hover .orb-tip,
.orb--sub:focus-visible .orb-tip {
  opacity: 1;
  transform: translate(0, -50%);
}

.orb:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

@media (prefers-reduced-motion: reduce) {
  .orb,
  .orb--sub,
  .dock:not(.is-open) .orb--sub {
    transition: opacity var(--dur-fast) var(--ease-out);
    transition-delay: 0ms;
  }
}

@media (max-height: 480px) {
  .orb-tip {
    display: none;
  }
}
</style>
