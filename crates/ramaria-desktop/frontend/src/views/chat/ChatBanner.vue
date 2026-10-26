<script setup lang="ts">
/**
 * src/views/chat/ChatBanner.vue — 对话视图横幅
 *
 * 职责:
 * - 呈现只读 / 导入 / 索引 / 摘要失败 / 隐私 / 模型不可用六类提示
 * - 携带至多一个动作，索引构建中在底边显示进度
 *
 * 设计特点:
 * - 图标与底色由种类决定，文案由调用方从状态文案表取
 * - 进度用原生 progress 元素绘制，不写内联样式
 * - 同屏只渲染一条横幅，优先级由父级计算
 */
import BaseButton from '../../components/base/BaseButton.vue'
import type { ChatBannerKind } from './useChatBanner'

withDefaults(
  defineProps<{
    kind: ChatBannerKind
    title: string
    description: string
    action?: string
    percent?: number
  }>(),
  {
    action: '',
    percent: 0,
  },
)

defineEmits<{
  action: []
}>()

/** 图标路径（只取与语义相关的一枚，避免引入图标库） */
const ICONS: Record<ChatBannerKind, string> = {
  readonly: 'M4 11h16v10H4zM8 11V7a4 4 0 0 1 8 0v4',
  import: 'M4 4h16v6H4zM4 14h16v6H4z',
  'index-idle': 'M12 2 2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5',
  'index-building': 'M12 2v4M12 18v4M2 12h4M18 12h4M5 5l3 3M16 16l3 3M19 5l-3 3M8 16l-3 3',
  'index-failed': 'M12 3 2 20h20L12 3zM12 9v5M12 17h.01',
  'index-partial': 'M12 3 2 20h20L12 3zM12 9v5M12 17h.01',
  l1: 'M12 3 2 20h20L12 3zM12 9v5M12 17h.01',
  privacy: 'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
  llm: 'M12 3 2 20h20L12 3zM12 9v5M12 17h.01',
}
</script>

<template>
  <section class="banner" :data-kind="kind" role="status">
    <svg class="banner-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path :d="ICONS[kind]" />
    </svg>
    <div class="banner-text">
      <p class="banner-title">{{ title }}</p>
      <p class="banner-desc">{{ description }}</p>
    </div>
    <BaseButton v-if="action" variant="ghost" size="sm" @click="$emit('action')">
      {{ action }}
    </BaseButton>
    <progress
      v-if="kind === 'index-building'"
      class="banner-progress"
      :value="percent"
      max="100"
    />
  </section>
</template>

<style scoped>
.banner {
  /* 底边进度条高度；圆角取一半，做成胶囊端头 */
  --banner-bar-height: 2px;

  position: relative;
  display: flex;
  align-items: center;
  gap: var(--space-10);
  min-height: var(--banner-h);
  padding: var(--space-8) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  overflow: hidden;
}

/* 信息 / 进行中走辅色蓝；未建立、失败、部分缺失仍走警示黄 */
.banner[data-kind='readonly'],
.banner[data-kind='index-building'] {
  border-color: var(--accent-line);
  background: var(--accent-surface);
}

.banner[data-kind='index-building'] .banner-desc {
  color: var(--text-2);
}

.banner[data-kind='import'] {
  border-color: var(--brand-line);
  background: var(--brand-surface);
}

.banner[data-kind='index-idle'],
.banner[data-kind='index-failed'],
.banner[data-kind='index-partial'],
.banner[data-kind='l1'],
.banner[data-kind='privacy'],
.banner[data-kind='llm'] {
  border-color: var(--warn-line);
  background: var(--warn-surface);
}

.banner-icon {
  flex: none;
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
  color: var(--text-3);
}

.banner[data-kind='readonly'] .banner-icon,
.banner[data-kind='index-building'] .banner-icon {
  color: var(--accent-text);
}

.banner[data-kind='import'] .banner-icon {
  color: var(--brand-text);
}

.banner[data-kind='index-idle'] .banner-icon,
.banner[data-kind='index-failed'] .banner-icon,
.banner[data-kind='index-partial'] .banner-icon,
.banner[data-kind='l1'] .banner-icon,
.banner[data-kind='privacy'] .banner-icon,
.banner[data-kind='llm'] .banner-icon {
  color: var(--warn);
}

.banner-text {
  flex: 1;
  min-width: 0;
}

.banner-title {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-small);
}

.banner-desc {
  margin-top: var(--space-4);
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
}

.banner-progress {
  position: absolute;
  inset: auto 0 0 0;
  width: 100%;
  height: var(--banner-bar-height);
  border: none;
  border-radius: calc(var(--banner-bar-height) / 2);
  appearance: none;
}

.banner-progress::-webkit-progress-bar {
  background: transparent;
  border-radius: calc(var(--banner-bar-height) / 2);
}

.banner-progress::-webkit-progress-value {
  background: var(--accent);
  border-radius: calc(var(--banner-bar-height) / 2);
}

.banner :deep(.btn--sm) {
  flex: none;
}
</style>
