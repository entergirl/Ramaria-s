<script setup lang="ts">
/**
 * src/views/chat/ChatSessionCard.vue — 会话卡片
 *
 * 职责:
 * - 列出会话（今天 / 更早分组），提供搜索、切换与新建入口
 * - 展示来源标签、未读计数、副行信息与时间
 *
 * 设计特点:
 * - 卡片贴着对话头部展开，不遮挡其它视图；点外部 / Esc / 再点头像由父级收起
 * - 打开时焦点落在搜索框，键盘可直接筛选
 * - 加载、空、错误三态在卡片内就地呈现，不切换视图
 */
import { computed, nextTick, onMounted, ref } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import {
  buildSessionRows,
  filterSessionRows,
  groupSessionRows,
} from '../../core/chat/sessionList'
import { useNow } from '../../core/useNow'
import { useSessionStore } from '../../stores/session'
import ChatAvatar from './ChatAvatar.vue'

const emit = defineEmits<{
  select: [sessionId: string]
  create: []
  close: []
  retry: []
}>()

const session = useSessionStore()
const now = useNow()
const search = ref('')
const searchInput = ref<HTMLInputElement | null>(null)

const rows = computed(() =>
  buildSessionRows(session.items, (uid) => session.personaOf(uid), now.value),
)

const filtered = computed(() => filterSessionRows(rows.value, search.value))
const groups = computed(() => groupSessionRows(filtered.value, now.value))
const isEmpty = computed(() => filtered.value.length === 0 && !session.loadingList)
const isFiltered = computed(() => search.value.trim() !== '')

onMounted(() => {
  void nextTick(() => searchInput.value?.focus({ preventScroll: true }))
})

function unreadText(count: number): string {
  return count > 99 ? '99+' : String(count)
}
</script>

<template>
  <aside id="chat-sessions" class="session-card" role="dialog" aria-label="会话列表">
    <header class="card-head">
      <span class="card-title">会话</span>
      <BaseButton variant="ghost" size="sm" @click="emit('create')">新建</BaseButton>
    </header>

    <div class="session-scroll">
      <div v-if="session.loadingList && rows.length === 0" class="loading">
        <BaseSkeleton v-for="index in 3" :key="index" shape="card" :lines="1" />
      </div>

      <div v-else-if="session.error && rows.length === 0" class="card-error" role="alert">
        <p class="error-text">{{ session.error }}</p>
        <BaseButton variant="secondary" size="sm" @click="emit('retry')">重试</BaseButton>
      </div>

      <p v-else-if="isEmpty" class="card-empty">
        {{ isFiltered ? '没有匹配的会话' : '暂无会话，发送消息后自动记录' }}
      </p>

      <template v-else>
        <template v-if="groups.today.length">
          <p class="group-label">今天</p>
          <button
            v-for="row in groups.today"
            :key="row.session.id"
            type="button"
            class="session-row"
            :data-active="row.session.id === session.activeId ? '' : undefined"
            @click="emit('select', row.session.id)"
          >
            <ChatAvatar :initial="row.initial" :tone="row.readonly ? 'accent' : 'brand'" />
            <span class="row-info">
              <span class="row-name">
                <span class="row-name-text">{{ row.name }}</span>
                <BaseTag v-for="tag in row.tags" :key="tag.label" :tone="tag.tone">
                  {{ tag.label }}
                </BaseTag>
              </span>
              <span class="row-preview">{{ row.preview }}</span>
            </span>
            <span class="row-meta">
              <span class="row-time">{{ row.timeText }}</span>
              <span v-if="row.unread > 0" class="row-badge">{{ unreadText(row.unread) }}</span>
            </span>
          </button>
        </template>

        <template v-if="groups.earlier.length">
          <p class="group-label">更早</p>
          <button
            v-for="row in groups.earlier"
            :key="row.session.id"
            type="button"
            class="session-row"
            :data-active="row.session.id === session.activeId ? '' : undefined"
            @click="emit('select', row.session.id)"
          >
            <ChatAvatar :initial="row.initial" :tone="row.readonly ? 'accent' : 'brand'" />
            <span class="row-info">
              <span class="row-name">
                <span class="row-name-text">{{ row.name }}</span>
                <BaseTag v-for="tag in row.tags" :key="tag.label" :tone="tag.tone">
                  {{ tag.label }}
                </BaseTag>
              </span>
              <span class="row-preview">{{ row.preview }}</span>
            </span>
            <span class="row-meta">
              <span class="row-time">{{ row.timeText }}</span>
              <span v-if="row.unread > 0" class="row-badge">{{ unreadText(row.unread) }}</span>
            </span>
          </button>
        </template>
      </template>
    </div>

    <div class="session-search">
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="11" cy="11" r="7" />
        <path d="m20 20-3.5-3.5" />
      </svg>
      <input
        ref="searchInput"
        v-model="search"
        type="text"
        class="search-input"
        placeholder="搜索会话"
        aria-label="搜索会话"
        @keydown.esc="emit('close')"
      />
    </div>
  </aside>
</template>

<style scoped>
.session-card {
  position: absolute;
  top: 86px;
  bottom: 74px;
  left: 4px;
  z-index: var(--z-sticky);
  display: flex;
  flex-direction: column;
  width: 292px;
  padding: var(--space-10);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-lg);
  animation: card-in var(--dur-base) var(--ease-out);
}

@keyframes card-in {
  from {
    opacity: 0;
    transform: translateY(-6px) scale(0.98);
  }
}

.card-head {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: 0 0 var(--space-8);
}

.card-title {
  flex: 1;
  font-size: var(--fs-body);
  font-weight: var(--fw-bold);
}

.session-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: var(--space-4) 0;
}

.group-label {
  padding: var(--space-10) var(--space-8) var(--space-6);
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  letter-spacing: 0.04em;
}

.session-row {
  display: flex;
  align-items: flex-start;
  gap: var(--space-10);
  width: 100%;
  padding: var(--space-8) var(--space-10);
  border: 1px solid transparent;
  border-radius: var(--radius-session);
  background: transparent;
  text-align: left;
  cursor: pointer;
  transition:
    background-color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out);
}

.session-row:hover {
  background: var(--bg-subtle);
}

.session-row:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.session-row[data-active] {
  background: var(--brand-surface);
  border-color: var(--brand-line);
}

.row-info {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.row-name {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
  min-width: 0;
}

.row-name-text {
  overflow: hidden;
  font-size: var(--fs-small);
  font-weight: var(--fw-bold);
  line-height: var(--lh-small);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row-preview {
  overflow: hidden;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row-meta {
  display: flex;
  flex: none;
  flex-direction: column;
  align-items: flex-end;
  gap: var(--space-6);
}

.row-time {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.row-badge {
  display: grid;
  place-items: center;
  min-width: 16px;
  height: 16px;
  padding: 0 var(--space-4);
  border-radius: var(--radius-full);
  background: var(--brand);
  color: var(--bg-card);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
}

.session-search {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-8);
  margin-top: var(--space-8);
  padding: var(--space-8) 0 0;
  border-top: 1px solid var(--border);
}

.session-search svg {
  flex: none;
  width: 14px;
  height: 14px;
  fill: none;
  stroke: var(--text-3);
  stroke-width: 2;
  stroke-linecap: round;
}

.search-input {
  flex: 1;
  min-width: 0;
  height: 30px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-family: inherit;
  font-size: var(--fs-small);
}

.search-input::placeholder {
  color: var(--text-3);
}

.search-input:focus-visible {
  outline: none;
  border-color: var(--brand);
  box-shadow: var(--focus-ring);
}

.loading {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  padding: var(--space-8);
}

.card-error {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-8);
  padding: var(--space-16) var(--space-10);
}

.error-text {
  color: var(--danger);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.card-empty {
  padding: var(--space-32) var(--space-12);
  color: var(--text-3);
  font-size: var(--fs-small);
  text-align: center;
}

@media (prefers-reduced-motion: reduce) {
  .session-card {
    animation: none;
  }
}
</style>
