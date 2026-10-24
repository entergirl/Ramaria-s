<script setup lang="ts">
/**
 * src/views/persona/PersonaToolbar.vue — 人格页工具栏
 *
 * 职责:
 * - 展示人格数量与默认人格数量，承载「导入预设」入口
 *
 * 设计特点:
 * - 左侧标题与统计两行排布，工具栏始终全宽
 * - 统计行随列表主态切换为「加载中 / 加载失败」，不显示过期数字
 * - 导入入口在列表请求中禁用，避免与列表刷新互相干扰
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'

const props = defineProps<{
  count: number
  defaultCount: number
  state: 'default' | 'loading' | 'empty' | 'error'
  busy: boolean
}>()

defineEmits<{
  import: []
}>()

const line = computed(() => {
  if (props.state === 'loading') {
    return '加载中…'
  }
  if (props.state === 'error') {
    return '加载失败'
  }
  return `${props.count} 个 · ${props.defaultCount} 个默认`
})
</script>

<template>
  <section class="toolbar">
    <div class="head">
      <h1 class="title">人格</h1>
      <p class="sub">{{ line }}</p>
    </div>

    <span class="spacer" />

    <BaseButton variant="primary" size="sm" :disabled="busy" @click="$emit('import')">
      <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 3v12" />
        <path d="m7 10 5 5 5-5" />
        <path d="M5 21h14" />
      </svg>
      导入预设
    </BaseButton>
  </section>
</template>

<style scoped>
.toolbar {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-12);
  padding: var(--space-14) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.head {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.title {
  color: var(--text-1);
  font-size: var(--fs-h2);
  font-weight: var(--fw-bold);
  line-height: 1.2;
  letter-spacing: -0.01em;
}

.sub {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.spacer {
  flex: 1;
}

.icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

@media (max-width: 720px) {
  .toolbar {
    flex-wrap: wrap;
  }
}
</style>
