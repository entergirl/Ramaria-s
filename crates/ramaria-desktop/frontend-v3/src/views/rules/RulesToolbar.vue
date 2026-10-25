<script setup lang="ts">
/**
 * src/views/rules/RulesToolbar.vue — 规则页工具栏
 *
 * 职责:
 * - 展示当前人格的头像与分类，作为人格卡的展开入口
 * - 展示规则总数并承载刷新动作
 *
 * 设计特点:
 * - 统计行随列表主态切换为「加载中 / 加载失败」，不显示过期数字
 * - 刷新键在请求中进入加载态并锁定，避免重入
 * - 工具栏始终全宽，不随人格卡收缩
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import PersonaFilterButton from '../../components/shell/PersonaFilterButton.vue'
import type { PersonaSummary } from '../../core/api/dto'

const props = defineProps<{
  persona: PersonaSummary | null
  personaOpen: boolean
  count: number
  state: 'default' | 'loading' | 'empty' | 'error'
  busy: boolean
}>()

defineEmits<{
  toggle: []
  refresh: []
}>()

const summary = computed(() => {
  if (props.state === 'loading') {
    return '加载中…'
  }
  if (props.state === 'error') {
    return '加载失败'
  }
  return `共 ${props.count} 条`
})
</script>

<template>
  <section class="toolbar">
    <PersonaFilterButton
      id="rules-persona-trigger"
      controls="rules-personas"
      :persona="persona"
      :open="personaOpen"
      @toggle="$emit('toggle')"
    />

    <span class="spacer" />

    <span class="count">{{ summary }}</span>
    <BaseButton
      variant="ghost"
      size="sm"
      icon-only
      :loading="busy"
      :disabled="persona === null"
      aria-label="刷新规则"
      @click="$emit('refresh')"
    >
      <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
        <polyline points="23 4 23 10 17 10" />
        <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
      </svg>
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

.spacer {
  flex: 1;
}

.count {
  color: var(--text-3);
  font-size: var(--fs-small);
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
