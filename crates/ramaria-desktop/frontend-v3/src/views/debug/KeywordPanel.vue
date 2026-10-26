<script setup lang="ts">
/**
 * src/views/debug/KeywordPanel.vue — 关键词池页签
 *
 * 职责:
 * - 展示三态计数、待确认别名裁决区与按状态分组的全量词条
 *
 * 设计特点:
 * - 待确认别名单独置顶，两个动作就地裁决，处理中只锁该行
 * - 分组默认只展开规范词，其余折叠；组内不做截断，总数始终可见
 * - 词条文本等宽字体呈现，指向的规范词以箭头跟随，使用次数右对齐
 */

import { computed, ref } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { KeywordPool, PendingAlias } from '../../core/api/dto'
import { groupKeywords } from '../../core/debug/format'
import DebugStat from './DebugStat.vue'

const props = defineProps<{
  pool: KeywordPool | null
  pending: PendingAlias[]
  resolving: Record<string, boolean>
}>()

const emit = defineEmits<{
  resolve: [alias: string, action: 'confirm' | 'reject']
}>()

const groups = computed(() => groupKeywords(props.pool?.keywords ?? []))

/** 分组展开态：初值取元信息里的默认展开 */
const openGroups = ref<Record<string, boolean>>(
  Object.fromEntries(groups.value.map((group) => [group.status, group.openByDefault])),
)

function isOpen(status: string): boolean {
  return openGroups.value[status] === true
}

function toggleGroup(status: string): void {
  openGroups.value = { ...openGroups.value, [status]: !isOpen(status) }
}

const canonicalCount = computed(() => props.pool?.canonical_count ?? 0)
const aliasCount = computed(() => props.pool?.alias_count ?? 0)
const pendingCount = computed(() => props.pool?.pending_count ?? props.pending.length)
</script>

<template>
  <div class="kw">
    <div class="stats">
      <DebugStat label="规范词" :value="canonicalCount" tone="brand" />
      <DebugStat label="已确认别名" :value="aliasCount" tone="accent" />
      <DebugStat label="待确认别名" :value="pendingCount" tone="warn" />
    </div>

    <section v-if="pending.length > 0" class="block">
      <header class="block-head">
        <h3 class="block-title">待确认别名冲突</h3>
        <BaseTag tone="warn">{{ pending.length }} 条</BaseTag>
      </header>
      <ul class="alias-list">
        <li v-for="item in pending" :key="item.alias_id" class="alias-row">
          <span class="alias-pair">
            <code class="code">{{ item.alias_keyword }}</code>
            <span class="arrow">→</span>
            <code class="code">{{ item.canonical_keyword }}</code>
          </span>
          <span class="alias-actions">
            <BaseButton
              variant="secondary"
              size="sm"
              :loading="resolving[item.alias_keyword] === true"
              @click="emit('resolve', item.alias_keyword, 'confirm')"
            >
              确认合并
            </BaseButton>
            <BaseButton
              variant="ghost"
              size="sm"
              :disabled="resolving[item.alias_keyword] === true"
              @click="emit('resolve', item.alias_keyword, 'reject')"
            >
              驳回为规范词
            </BaseButton>
          </span>
        </li>
      </ul>
    </section>

    <section class="block">
      <header class="block-head">
        <h3 class="block-title">词条分层</h3>
        <span class="block-note">共 {{ pool?.total ?? 0 }} 条</span>
      </header>

      <div v-for="group in groups" :key="group.status" class="group">
        <button
          type="button"
          class="group-head"
          :aria-expanded="isOpen(group.status)"
          @click="toggleGroup(group.status)"
        >
          <svg class="chevron" :class="{ 'is-open': isOpen(group.status) }" viewBox="0 0 12 12" aria-hidden="true">
            <path d="M4 2.5L7.5 6L4 9.5" />
          </svg>
          <span class="group-title">{{ group.title }}</span>
          <BaseTag :tone="group.tone">{{ group.items.length }}</BaseTag>
        </button>

        <ul v-if="isOpen(group.status)" class="keyword-list">
          <li v-for="item in group.items" :key="`${group.status}-${item.keyword}`" class="keyword-row">
            <code class="code">{{ item.keyword }}</code>
            <span v-if="item.canonical_keyword" class="arrow">→ {{ item.canonical_keyword }}</span>
            <span class="keyword-count">使用 {{ item.use_count }} 次</span>
          </li>
          <li v-if="group.items.length === 0" class="keyword-note">该状态下暂无词条</li>
        </ul>
      </div>
    </section>
  </div>
</template>

<style scoped>
.kw {
  display: flex;
  flex-direction: column;
  gap: var(--space-14);
}

.stats {
  display: flex;
  gap: var(--gap-card);
}

.block {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  padding: var(--pad-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  box-shadow: var(--shadow-sm);
}

.block-head {
  display: flex;
  align-items: center;
  gap: var(--space-8);
}

.block-title {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.block-note {
  margin-left: auto;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.alias-list,
.keyword-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  margin: 0;
  padding: 0;
  list-style: none;
}

.alias-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.alias-pair {
  display: inline-flex;
  flex: 1;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
  min-width: 0;
}

.alias-actions {
  display: inline-flex;
  flex: none;
  gap: var(--space-6);
}

.group {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.group-head {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-6) var(--space-8);
  border: none;
  border-radius: var(--radius-btn-sm);
  background: transparent;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  text-align: left;
  cursor: pointer;
  transition: background-color var(--dur-fast) var(--ease-out);
}

.group-head:hover {
  background: var(--bg-subtle);
}

.group-head:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.chevron {
  width: 12px;
  height: 12px;
  fill: none;
  stroke: var(--text-3);
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
  transition: transform var(--dur-fast) var(--ease-out);
}

.chevron.is-open {
  transform: rotate(90deg);
}

.group-title {
  margin-right: var(--space-4);
}

.keyword-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-4) var(--space-10);
  border-radius: var(--radius-btn-sm);
  color: var(--text-1);
  font-size: var(--fs-small);
}

.keyword-row:nth-child(odd) {
  background: var(--bg-subtle);
}

.code {
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  overflow-wrap: anywhere;
}

.arrow {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.keyword-count {
  margin-left: auto;
  color: var(--text-3);
  font-size: var(--fs-caption);
  white-space: nowrap;
}

.keyword-note {
  padding: var(--space-6) var(--space-10);
  color: var(--text-3);
  font-size: var(--fs-caption);
}
</style>
