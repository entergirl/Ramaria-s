<script setup lang="ts">
/**
 * src/components/shell/PersonaPicker.vue — 人格卡（筛选用）
 *
 * 职责:
 * - 在内容列左侧列出可筛选的人格，并标出当前项
 *
 * 设计特点:
 * - 文档流内卡片，不是侧栏；高度随右侧内容拉伸
 * - 每行只有头像、名称与分类，当前项用品牌底色与描边
 * - 行是按钮，当前行 aria-current；分类标签不承担操作
 * - 记忆页与规则页共用，DOM id 由调用方给出
 */
import { computed } from 'vue'

import BaseTag from '../base/BaseTag.vue'
import type { PersonaSummary } from '../../core/api/dto'
import { personaCategory, personaInitial } from '../../core/memory/format'

const props = defineProps<{
  /** 人格卡的 DOM id（工具栏 aria-controls 指向） */
  id: string
  personas: PersonaSummary[]
  activeUid: string | null
}>()

defineEmits<{
  select: [uid: string]
}>()

const rows = computed(() =>
  props.personas.map((persona) => ({
    persona,
    initial: personaInitial(persona.name),
    category: personaCategory(persona.kind, persona.source),
    accent: persona.kind === 'user',
  })),
)
</script>

<template>
  <aside :id="id" class="persona-menu" role="dialog" aria-label="人格列表">
    <button
      v-for="row in rows"
      :key="row.persona.uid"
      type="button"
      class="persona-row"
      :class="{ 'is-active': row.persona.uid === activeUid }"
      :aria-current="row.persona.uid === activeUid ? 'true' : undefined"
      @click="$emit('select', row.persona.uid)"
    >
      <span class="avatar" :class="{ 'avatar-accent': row.accent }">{{ row.initial }}</span>
      <span class="persona-row-name">{{ row.persona.name }}</span>
      <BaseTag :tone="row.category.tone" class="persona-row-cat">{{ row.category.label }}</BaseTag>
    </button>
  </aside>
</template>

<style scoped>
.persona-menu {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
  padding: var(--space-10);
  /* 纵向可滚：人格多时在卡内滚动，横向隐藏避免抽屉展开动画露边 */
  overflow-x: hidden;
  overflow-y: auto;
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.persona-row {
  display: flex;
  width: 100%;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-8);
  border: 1px solid transparent;
  border-radius: var(--radius-input);
  background: transparent;
  text-align: left;
  cursor: pointer;
  transition:
    background-color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out);
}

.persona-row:hover {
  background: var(--bg-subtle);
}

.persona-row:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.persona-row.is-active {
  background: var(--brand-surface);
  border-color: var(--brand-line);
}

.avatar {
  display: grid;
  flex: none;
  place-items: center;
  width: 30px;
  height: 30px;
  border: 1px solid var(--brand-line);
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-size: var(--fs-small);
  font-weight: var(--fw-bold);
  line-height: 1;
}

.avatar-accent {
  border-color: var(--accent-line);
  background: var(--accent-surface);
  color: var(--accent-text);
}

.persona-row-name {
  min-width: 0;
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.persona-row-cat {
  margin-left: auto;
  flex: none;
}
</style>
