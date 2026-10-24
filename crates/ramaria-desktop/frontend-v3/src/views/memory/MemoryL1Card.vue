<script setup lang="ts">
/**
 * src/views/memory/MemoryL1Card.vue — L1 会话摘要卡
 *
 * 职责:
 * - 展示摘要、时段 / 氛围 / 参与人数、关键词、效价与强度
 * - 卡内展开来源会话摘录与采集信息，并提供回对话入口
 *
 * 设计特点:
 * - 摘要正文与关键词行左对齐；详情与收起用同一个按钮切换
 * - 时间以发生时间为准，悬停补入库时间；来源摘录里的撤回消息只留占位
 * - 详情不弹窗、不抽屉，展开时只影响本卡高度
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import type { MemoryL1 } from '../../core/api/dto'
import { formatClock, formatDateTime, smartTime } from '../../core/chat/time'
import {
  dayLabel,
  decimal,
  limitedKeywords,
  signedDecimal,
  valenceTone,
} from '../../core/memory/format'
import type { ExcerptState } from './useMemoryView'

const props = defineProps<{
  item: MemoryL1
  open: boolean
  participants: string[]
  messageCount: number | null
  excerpt: ExcerptState | undefined
  personaName: string
}>()

defineEmits<{
  toggle: []
  'open-chat': []
  'jump-l2': []
  'retry-excerpt': []
}>()

const timeText = computed(() => smartTime(props.item.occurred_at || props.item.created_at))
const occurredText = computed(() =>
  formatDateTime(props.item.occurred_at || props.item.created_at),
)
const createdText = computed(() => formatDateTime(props.item.created_at))
const generatedText = computed(() => smartTime(props.item.created_at))
const keywords = computed(() => limitedKeywords(props.item.keywords))
/** 来源摘录只取对话双方，system / tool 不入摘录 */
const excerptMessages = computed(
  () =>
    props.excerpt?.messages.filter(
      (message) => message.role === 'user' || message.role === 'assistant',
    ) ?? [],
)
const tone = computed(() => valenceTone(props.item.valence))
const note = computed(() =>
  props.messageCount
    ? `摘要由 ${props.messageCount} 条消息生成；可在对话页回溯原文。`
    : '可在对话页回溯原文。',
)

/** 摘录行的发言人：用户统一「你」，其余取平台显示名或当前人格名 */
function senderOf(role: string, senderName: string | null): string {
  if (role === 'user') {
    return '你'
  }
  return senderName && senderName.length > 0 ? senderName : props.personaName
}
</script>

<template>
  <article class="mem-card" :class="{ 'is-open': open }">
    <p class="mem-summary">{{ item.summary }}</p>

    <div class="meta-row">
      <span>{{ dayLabel(item.occurred_at || item.created_at) }}</span>
      <span v-if="item.time_period">{{ item.time_period }}</span>
      <span v-if="item.atmosphere">氛围 · {{ item.atmosphere }}</span>
      <span v-if="participants.length">{{ participants.length }} 人参与</span>
    </div>

    <div v-if="keywords.length" class="chip-row">
      <span v-for="keyword in keywords" :key="keyword" class="chip-sm">{{ keyword }}</span>
    </div>

    <div class="meta-row">
      <span class="kv">效价 <b :class="`valence-${tone}`">{{ signedDecimal(item.valence) }}</b></span>
      <span class="kv">强度 <b>{{ decimal(item.salience) }}</b></span>
      <span class="spacer" />
      <span class="time-note" :title="`入库：${createdText}`">{{ timeText }}</span>
      <BaseButton variant="ghost" size="sm" @click="$emit('open-chat')">查看对话</BaseButton>
      <BaseButton
        variant="ghost"
        size="sm"
        :aria-expanded="open ? 'true' : 'false'"
        :aria-controls="`l1-detail-${item.id}`"
        @click="$emit('toggle')"
      >
        {{ open ? '收起' : '详情' }}
        <svg class="chev" :class="{ 'is-up': open }" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </BaseButton>
    </div>

    <div v-if="open" :id="`l1-detail-${item.id}`" class="mem-detail">
      <div class="detail-cols">
        <section class="detail-block">
          <div class="detail-title">来源会话</div>
          <p v-if="!excerpt || excerpt.loading" class="detail-hint">读取中…</p>
          <div v-else-if="excerpt.error" class="detail-error">
            <span>{{ excerpt.error }}</span>
            <BaseButton variant="ghost" size="sm" @click="$emit('retry-excerpt')">重试</BaseButton>
          </div>
          <div v-else-if="excerptMessages.length === 0" class="detail-hint">
            该会话没有可显示的原文。
          </div>
          <div v-else class="source-excerpt">
            <div
              v-for="message in excerptMessages"
              :key="message.id"
              class="src-row"
              :class="{ 'is-user': message.role === 'user' }"
            >
              <span class="who">{{ senderOf(message.role, message.sender_name) }}</span>
              <span class="txt" :class="{ 'is-recalled': message.recalled_at !== null }">
                {{ message.recalled_at !== null ? '消息已撤回' : message.content }}
              </span>
              <span class="at">{{ formatClock(message.created_at) }}</span>
            </div>
          </div>
        </section>

        <section class="detail-block">
          <div class="detail-title">采集</div>
          <div class="detail-kv"><span class="k">时间</span><b>{{ occurredText }}</b></div>
          <div class="detail-kv">
            <span class="k">参与者</span><b>{{ participants.join('、') || '—' }}</b>
          </div>
          <div class="detail-kv">
            <span class="k">消息</span>
            <b>{{ messageCount ? `${messageCount} 条 → 1 条摘要` : '未记录' }}</b>
          </div>
          <div class="detail-kv">
            <span class="k">效价 / 强度</span>
            <b>{{ signedDecimal(item.valence) }} / {{ decimal(item.salience) }}</b>
          </div>
          <div class="detail-kv"><span class="k">生成</span><b>L1 · {{ generatedText }}</b></div>
        </section>
      </div>

      <section class="detail-block">
        <div class="detail-title">关联</div>
        <div class="chip-row">
          <button type="button" class="chip-link" @click="$emit('jump-l2')">查看 L2 事件</button>
        </div>
      </section>

      <div class="detail-actions">
        <BaseButton variant="ghost" size="sm" @click="$emit('open-chat')">查看完整对话</BaseButton>
        <span class="spacer" />
        <span class="detail-note">{{ note }}</span>
      </div>
    </div>
  </article>
</template>

<style scoped>
.mem-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  padding: var(--pad-card-lg);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.mem-card.is-open {
  border-color: var(--brand-line);
  box-shadow: var(--shadow-md);
}

.mem-summary {
  font-size: var(--fs-body);
  line-height: 1.72;
}

.meta-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.meta-row .kv b {
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.meta-row .kv b.valence-pos {
  color: var(--ok);
}

.meta-row .kv b.valence-neg {
  color: var(--danger);
}

.meta-row .kv b.valence-flat {
  color: var(--text-2);
}

.time-note {
  cursor: default;
}

.chev {
  width: 11px;
  height: 11px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2.2;
  stroke-linecap: round;
  stroke-linejoin: round;
  transition: transform var(--dur-fast) var(--ease-out);
}

.chev.is-up {
  transform: rotate(180deg);
}

.chip-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-6);
}

.chip-sm {
  display: inline-flex;
  align-items: center;
  height: 20px;
  padding: 0 var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-size: var(--fs-caption);
  line-height: 1;
}

.chip-link {
  height: 22px;
  padding: 0 var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-card);
  color: var(--brand-text);
  font-family: inherit;
  font-size: var(--fs-caption);
  cursor: pointer;
}

.chip-link:hover {
  border-color: var(--brand-line);
  background: var(--brand-surface);
}

.chip-link:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.mem-detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  padding-top: var(--space-12);
  border-top: 1px dashed var(--border);
}

.detail-cols {
  display: grid;
  grid-template-columns: 1.15fr 0.85fr;
  gap: var(--space-12);
  align-items: start;
}

.detail-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  min-width: 0;
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.detail-title {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  letter-spacing: 0.06em;
}

.detail-hint {
  color: var(--text-3);
  font-size: var(--fs-small);
}

.detail-error {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  color: var(--danger);
  font-size: var(--fs-small);
}

.detail-kv {
  display: flex;
  gap: var(--space-8);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.detail-kv .k {
  flex: none;
  width: 64px;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.detail-kv b {
  min-width: 0;
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.source-excerpt {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.src-row {
  display: flex;
  gap: var(--space-8);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.src-row .who {
  flex: none;
  width: 34px;
  padding-top: 1px;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.src-row .txt {
  flex: 1;
  min-width: 0;
  color: var(--text-2);
}

.src-row.is-user .txt {
  color: var(--brand-text);
}

.src-row .txt.is-recalled {
  color: var(--text-3);
  font-style: italic;
}

.src-row .at {
  flex: none;
  margin-left: auto;
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.detail-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.detail-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.spacer {
  flex: 1;
}

@media (max-width: 760px) {
  .detail-cols {
    grid-template-columns: 1fr;
  }
}
</style>
