<script setup lang="ts">
/**
 * src/views/settings/custom/ProactiveOverviewCard.vue — 主动投递概览卡
 *
 * 职责:
 * - 展示今日 / 本周投递、最近投递与冷却、退避与判据累计、回应率
 *
 * 设计特点:
 * - 只呈现后端可观测字段；调度内部状态（等待空闲 / 下次窗口）不猜
 * - 今日与本周由按日聚合数据在前端换算，日期口径与后端本地日期一致
 * - 无投递记录时给空态说明，不显示零值表格
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { proactiveApi } from '../../../core/api'
import type { ProactiveOverview } from '../../../core/api/dto'
import { errorMessage } from '../../../core/errors'
import SettingsCard from '../SettingsCard.vue'

const stats = ref<ProactiveOverview | null>(null)
const loading = ref(false)
const error = ref('')

/** 本地日期字符串（`YYYY-MM-DD`），`offsetDays` 为负表示往前 */
function localDate(offsetDays = 0): string {
  const date = new Date()
  date.setDate(date.getDate() + offsetDays)
  const year = date.getFullYear()
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${year}-${month}-${day}`
}

function formatTime(value: number): string {
  const date = new Date(value)
  const time = date.toLocaleTimeString('zh-CN', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
  })
  const monthDay = `${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
  return monthDay === localDate().slice(5) ? `今天 ${time}` : `${monthDay} ${time}`
}

const todayCount = computed(() => {
  if (!stats.value) {
    return 0
  }
  return stats.value.daily_date === localDate() ? stats.value.daily_count : 0
})

const weekCount = computed(() => {
  if (!stats.value) {
    return 0
  }
  const since = localDate(-6)
  return stats.value.daily
    .filter((row) => row.date >= since)
    .reduce((sum, row) => sum + row.count, 0)
})

const hasData = computed(() => (stats.value?.totals.deliveries ?? 0) > 0)

const lastSentText = computed(() =>
  stats.value?.last_sent_at ? formatTime(stats.value.last_sent_at) : '暂无',
)

const cooldownText = computed(() => {
  if (!stats.value?.last_sent_at) {
    return '暂无'
  }
  const until = stats.value.last_sent_at + stats.value.cooldown_hours * 3_600_000
  return until > Date.now() ? formatTime(until) : '已解除'
})

const responseText = computed(() => {
  const rate = stats.value?.totals.response_rate
  if (rate === null || rate === undefined) {
    return '暂无'
  }
  return `${Math.round(rate * 100)}%`
})

/** 回应中位时长：毫秒换成分 / 秒，无回应记「暂无」 */
const medianText = computed(() => {
  const median = stats.value?.totals.median_response_ms
  if (median === null || median === undefined) {
    return '暂无'
  }
  const minutes = Math.round(median / 60_000)
  return minutes >= 1 ? `${minutes} 分钟` : `${Math.max(1, Math.round(median / 1000))} 秒`
})

/** 近 7 天投递计数：日期短格式 + 条数，空档由后端补零 */
const dailyText = computed(() => {
  const rows = stats.value?.daily ?? []
  if (rows.length === 0) {
    return '暂无'
  }
  return rows
    .slice(-7)
    .map((row) => `${row.date.slice(5)} ${row.count}`)
    .join(' · ')
})

async function load(): Promise<void> {
  if (loading.value) {
    return
  }
  loading.value = true
  error.value = ''
  try {
    stats.value = await proactiveApi.getStats()
  } catch (cause) {
    error.value = errorMessage(cause, '主动对话统计读取失败')
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  void load()
})
</script>

<template>
  <SettingsCard
    title="投递概览"
    description="主动消息的投递与回应统计；调度实时状态不在其中。"
    :editable="false"
    :error="error"
  >
    <template #head>
      <BaseTag v-if="hasData" tone="ok">已投递 {{ stats?.totals.deliveries ?? 0 }} 次</BaseTag>
      <BaseTag v-else tone="neutral">暂无投递</BaseTag>
    </template>

    <template v-if="hasData">
      <div class="row">
        <span class="row-label">今日</span>
        <span class="row-value">
          {{ todayCount }} / {{ stats?.daily_limit ?? 0 }} 条
          <span v-if="stats && stats.daily_total_limit > 0" class="row-sub">
            （全部人格合计上限 {{ stats.daily_total_limit }}）
          </span>
        </span>
      </div>
      <div class="row">
        <span class="row-label">近 7 天</span>
        <span class="row-value">{{ weekCount }} 条</span>
      </div>
      <div class="row">
        <span class="row-label">最近一次</span>
        <span class="row-value">{{ lastSentText }}</span>
      </div>
      <div class="row">
        <span class="row-label">冷却至</span>
        <span class="row-value">{{ cooldownText }}</span>
      </div>
      <div class="row">
        <span class="row-label">回应</span>
        <span class="row-value">
          {{ stats?.totals.responded ?? 0 }} 次 · {{ responseText }}
        </span>
      </div>
      <div class="row">
        <span class="row-label">判据</span>
        <span class="row-value">
          可投递 {{ stats?.judge_yes_count ?? 0 }} · 不投递 {{ stats?.judge_no_count ?? 0 }}
          <span v-if="stats?.last_judge_at" class="row-sub">（最近 {{ formatTime(stats.last_judge_at) }}）</span>
        </span>
      </div>
      <div class="row">
        <span class="row-label">沉默计数</span>
        <span class="row-value">{{ stats?.silence_streak_max ?? 0 }}</span>
      </div>
      <div class="row">
        <span class="row-label">覆盖人格</span>
        <span class="row-value">{{ stats?.totals.personas ?? 0 }} 个</span>
      </div>
      <div class="row">
        <span class="row-label">回应中位</span>
        <span class="row-value">{{ medianText }}</span>
      </div>
      <div class="row">
        <span class="row-label">近 7 天</span>
        <span class="row-value daily">{{ dailyText }}</span>
      </div>
    </template>

    <p v-else class="note">还没有主动投递记录；满足空闲时长与时段条件后会在这里累计。</p>

    <template #footExtra>
      <BaseButton variant="ghost" size="sm" :loading="loading" @click="load">刷新</BaseButton>
    </template>
  </SettingsCard>
</template>

<style scoped>
.row {
  display: flex;
  align-items: baseline;
  gap: var(--space-10);
  font-size: var(--fs-small);
}

.row-label {
  flex: none;
  width: 64px;
  color: var(--text-3);
}

.row-value {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
  line-height: 1.6;
}

.row-sub {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.daily {
  color: var(--text-2);
  word-break: break-word;
}

.note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}
</style>
