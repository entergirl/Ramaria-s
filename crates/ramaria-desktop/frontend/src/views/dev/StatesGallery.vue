<script setup lang="ts">
/**
 * src/views/dev/StatesGallery.vue — 状态画廊
 *
 * 职责:
 * - 并排呈现基础组件的全部状态格与契约视图的六态缩略，供逐格走查与截图
 *
 * 设计特点:
 * - 只在开发构建中挂载，生产构建由路由层的常量分支整体剔除
 * - 可切浅色 / 深色与减弱动效，等效复现系统级偏好下的观感
 * - 每格带验收编号与触发方式，交互态由真实指针与键盘触发，不复制生产样式
 */
import { computed, nextTick, ref } from 'vue'
import { useRoute } from 'vue-router'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseSegmented from '../../components/base/BaseSegmented.vue'
import { applyTheme, currentTheme, type ThemeName } from '../../core/theme'
import {
  COMPONENT_CELL_COUNT,
  COMPONENT_ROWS,
  VIEW_CELL_COUNT,
  VIEW_ROWS,
} from './gallery/catalog'
import ContentCells from './gallery/ContentCells.vue'
import ControlCells from './gallery/ControlCells.vue'
import FeedbackCells from './gallery/FeedbackCells.vue'
import StateCell from './gallery/StateCell.vue'
import ViewStateThumb from './gallery/ViewStateThumb.vue'

// 查询参数用于截图取证：`?theme=dark&motion=reduced`，只改根节点与画廊标记，不写偏好
const route = useRoute()
/** 无路由上下文（脱离应用单独挂载）时按当前偏好与正常动效呈现 */
const query = route?.query ?? {}
const queryTheme = query.theme === 'dark' ? 'dark' : query.theme === 'light' ? 'light' : null
const theme = ref<ThemeName>(queryTheme ?? currentTheme())
const motion = ref<'normal' | 'reduced'>(query.motion === 'reduced' ? 'reduced' : 'normal')

if (queryTheme) {
  document.documentElement.dataset.theme = queryTheme
  document.documentElement.dataset.themeReady = 'true'
}

const componentsAnchor = ref<HTMLElement | null>(null)
const viewsAnchor = ref<HTMLElement | null>(null)

const themeOptions = [
  { value: 'light', label: '浅色' },
  { value: 'dark', label: '深色' },
]

const motionOptions = [
  { value: 'normal', label: '正常动效' },
  { value: 'reduced', label: '减弱动效' },
]

const total = computed(() => COMPONENT_CELL_COUNT + VIEW_CELL_COUNT)

function onThemeChange(value: string): void {
  theme.value = value === 'dark' ? 'dark' : 'light'
  applyTheme(theme.value)
}

function onMotionChange(value: string): void {
  motion.value = value === 'reduced' ? 'reduced' : 'normal'
}

async function jumpTo(target: 'components' | 'views'): Promise<void> {
  await nextTick()
  const node = target === 'components' ? componentsAnchor.value : viewsAnchor.value
  node?.scrollIntoView({ block: 'start' })
}
</script>

<template>
  <main class="gallery" :data-motion="motion === 'reduced' ? 'reduced' : undefined">
    <div class="gallery-inner">
      <header class="gallery-head">
        <div class="head-text">
          <h1 class="head-title">状态画廊</h1>
          <p class="head-note">
            基础组件 {{ COMPONENT_CELL_COUNT }} 格、视图缩略 {{ VIEW_CELL_COUNT }} 格，共 {{ total }} 格；
            交互态按「触发」提示用指针或键盘查看。视图缩略只示意状态载体，内容与文案以实机为准。
          </p>
        </div>
        <div class="head-controls">
          <BaseSegmented :model-value="theme" :options="themeOptions" label="主题" @update:model-value="onThemeChange" />
          <BaseSegmented
            :model-value="motion"
            :options="motionOptions"
            label="动效"
            @update:model-value="onMotionChange"
          />
          <div class="head-jump">
            <BaseButton size="sm" variant="ghost" @click="jumpTo('components')">组件</BaseButton>
            <BaseButton size="sm" variant="ghost" @click="jumpTo('views')">视图</BaseButton>
          </div>
        </div>
      </header>

      <section ref="componentsAnchor" class="block">
        <h2 class="block-title">组件状态</h2>
        <div v-for="row in COMPONENT_ROWS" :key="row.key" class="row" :data-row="row.key">
          <header class="row-head">
            <span class="row-no">{{ row.no }}</span>
            <span class="row-name">{{ row.name }}</span>
            <span class="row-qa">{{ row.qa }}</span>
            <span class="row-rule">{{ row.rule }}</span>
          </header>
          <div class="cells">
            <StateCell v-for="item in row.cells" :key="item.id" :cell="item" :code="row.qa">
              <ControlCells v-if="row.group === 'control'" :row="row.key" :cell="item.slug" />
              <ContentCells v-else-if="row.group === 'content'" :row="row.key" :cell="item.slug" />
              <FeedbackCells v-else :row="row.key" :cell="item.slug" />
            </StateCell>
          </div>
        </div>
      </section>

      <section ref="viewsAnchor" class="block">
        <h2 class="block-title">视图六态</h2>
        <div v-for="row in VIEW_ROWS" :key="row.key" class="row" :data-row="row.key">
          <header class="row-head">
            <span class="row-name">{{ row.name }}</span>
            <span class="row-qa">{{ row.qa }}</span>
            <span class="row-milestone">{{ row.milestone }}</span>
            <span class="row-rule">{{ row.rule }}</span>
          </header>
          <div class="cells cells--views">
            <StateCell v-for="item in row.cells" :key="item.id" :cell="item" :code="row.qa">
              <ViewStateThumb :view="row.key" :cell="item" />
            </StateCell>
          </div>
        </div>
      </section>
    </div>
  </main>
</template>

<style scoped>
.gallery {
  min-height: 100vh;
  padding: var(--space-40) var(--space-24) var(--space-48);
}

/* 减弱动效：时长归零并移除动画，无限循环元素停在各自的静态形态 */
.gallery[data-motion='reduced'] {
  --dur-fast: 0ms;
  --dur-base: 0ms;
  --dur-slow: 0ms;
  --dur-shimmer: 0ms;
}

.gallery[data-motion='reduced'] :deep(*) {
  animation: none !important;
}

.gallery-inner {
  display: flex;
  flex-direction: column;
  gap: var(--gap-section);
  width: min(1180px, 100%);
  margin: 0 auto;
}

.gallery-head {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  justify-content: space-between;
  gap: var(--space-16);
  padding-bottom: var(--space-16);
  border-bottom: 1px solid var(--border);
}

.head-text {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  max-width: 640px;
}

.head-title {
  color: var(--text-1);
  font-family: var(--font-title);
  font-size: var(--fs-h1);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h1);
}

.head-note {
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.head-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-12);
}

.head-jump {
  display: flex;
  gap: var(--space-4);
}

.block {
  display: flex;
  flex-direction: column;
  gap: var(--gap-section);
  scroll-margin-top: var(--space-24);
}

.block-title {
  color: var(--text-1);
  font-family: var(--font-title);
  font-size: var(--fs-h2);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h2);
}

.row {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
}

.row-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-8);
  padding-bottom: var(--space-6);
  border-bottom: 1px solid var(--border);
}

.row-no {
  display: grid;
  place-items: center;
  width: 20px;
  height: 20px;
  border-radius: var(--radius-check);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.row-name {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.row-qa {
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.row-milestone {
  padding: 0 var(--space-6);
  border: 1px solid var(--accent-line);
  border-radius: var(--radius-tag);
  background: var(--accent-surface);
  color: var(--accent-text);
  font-size: var(--fs-caption);
}

.row-rule {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
}

.cells {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(212px, 1fr));
  gap: var(--gap-card);
}

.cells--views {
  grid-template-columns: repeat(auto-fill, minmax(248px, 1fr));
}
</style>
