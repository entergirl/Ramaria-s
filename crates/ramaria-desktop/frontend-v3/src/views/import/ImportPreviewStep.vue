<script setup lang="ts">
/**
 * src/views/import/ImportPreviewStep.vue — 导入向导第二步（预览报告）
 *
 * 职责:
 * - 展示解析统计、双方标识、时间范围、导入模式与群聊成员分布
 * - 提供返回重选与确认导入
 *
 * 设计特点:
 * - 报告即确认页：不额外弹二次确认
 * - 导入模式与对话类型随信息列呈现，页头只留标题与文件名
 * - 信息列与统计卡同底（浅底面板）；页头标题内缩，与面板文字对齐
 * - 解析失败与解析中都不展示半份统计
 * - 切割间隔取后端返回的真实值，避免与表单当前值错位
 * - 动作置于右下：重新选择在左、确认导入在右
 */

import BaseButton from '../../components/base/BaseButton.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import type { ImportAnalysisReport } from '../../core/api/dto'
import type { ImportMetric } from '../../core/import/progress'
import { chatTypeLabel, timeRangeText } from '../../core/import/report'

defineProps<{
  report: ImportAnalysisReport | null
  analyzing: boolean
  error: string
  busy: boolean
  stats: ImportMetric[]
  members: string
  timeRange: string
  fileName: string
  modeText: string
}>()

const emit = defineEmits<{
  back: []
  confirm: []
  retry: []
}>()
</script>

<template>
  <div class="step-body">
    <template v-if="analyzing">
      <div class="loading-note" role="status">正在解析文件，暂不写入数据库…</div>
      <BaseSkeleton shape="card" :lines="4" />
      <BaseSkeleton shape="card" :lines="3" />
    </template>

    <BaseEmptyState
      v-else-if="error"
      variant="blocked"
      title="文件解析失败"
      description="没有写入任何数据；可以换一个文件，或排除问题后重试。"
    >
      <div class="state-slot">
        <p class="state-reason">{{ error }}</p>
        <div class="state-actions">
          <BaseButton variant="secondary" size="sm" :disabled="busy" @click="emit('retry')">
            重试解析
          </BaseButton>
          <BaseButton variant="ghost" size="sm" :disabled="busy" @click="emit('back')">
            重新选择
          </BaseButton>
        </div>
      </div>
    </BaseEmptyState>

    <template v-else-if="report">
      <header class="report-head">
        <div class="report-title">文件解析报告</div>
        <div class="report-file">{{ fileName }}</div>
      </header>

      <div class="stat-grid">
        <div v-for="item in stats" :key="item.key" class="stat">
          <div class="stat-label">{{ item.label }}</div>
          <div class="stat-value">{{ item.value }}</div>
        </div>
      </div>

      <dl class="detail-panel">
        <div class="detail-row">
          <dt>导出者</dt>
          <dd>
            {{ report.self_name || '未知' }}
            <code class="mono">{{ report.self_id }}</code>
            <span v-if="report.self_uin" class="muted">QQ {{ report.self_uin }}</span>
          </dd>
        </div>
        <div v-if="report.other_name" class="detail-row">
          <dt>对方</dt>
          <dd>
            {{ report.other_name }}
            <code class="mono">{{ report.other_uid }}</code>
            <span v-if="report.other_uin" class="muted">QQ {{ report.other_uin }}</span>
          </dd>
        </div>
        <div v-else class="detail-row">
          <dt>对话对象</dt>
          <dd>{{ report.chat_name || '未知' }}</dd>
        </div>
        <div class="detail-row">
          <dt>时间范围</dt>
          <dd>{{ timeRangeText(report.time_range) }}</dd>
        </div>
        <div class="detail-row">
          <dt>Session 切割</dt>
          <dd>{{ report.session_count }} 个会话（间隔 {{ report.gap_minutes }} 分钟）</dd>
        </div>
        <div class="detail-row">
          <dt>导入模式</dt>
          <dd>{{ modeText }}</dd>
        </div>
        <div class="detail-row">
          <dt>对话类型</dt>
          <dd>{{ chatTypeLabel(report.chat_type) }}</dd>
        </div>
      </dl>

      <div v-if="report.chat_type === 'group' && members" class="group-block">
        <div class="block-title">群聊成员（按消息数）</div>
        <p class="block-text">{{ members }}</p>
      </div>

      <div class="actions">
        <BaseButton variant="ghost" :disabled="busy" @click="emit('back')">重新选择</BaseButton>
        <BaseButton :disabled="busy" @click="emit('confirm')">确认导入</BaseButton>
      </div>
    </template>
  </div>
</template>

<style scoped>
.step-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-16);
}

.loading-note {
  color: var(--text-3);
  font-size: var(--fs-small);
}

.report-head {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding-left: var(--space-14);
}

.report-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.report-file {
  color: var(--text-3);
  font-size: var(--fs-caption);
  word-break: break-all;
}

.stat-grid {
  display: grid;
  grid-template-columns: repeat(5, 1fr);
  gap: var(--space-10);
}

.stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.stat-label {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.stat-value {
  color: var(--text-1);
  font-family: var(--font-display);
  font-size: var(--fs-h2);
  font-weight: var(--fw-semibold);
}

.detail-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  margin: 0;
  padding: var(--space-12) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.detail-row {
  display: grid;
  grid-template-columns: 96px 1fr;
  gap: var(--space-10);
  align-items: baseline;
  font-size: var(--fs-small);
}

.detail-row dt {
  color: var(--text-3);
}

.detail-row dd {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-8);
  margin: 0;
  color: var(--text-1);
}

.mono {
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.muted {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.group-block {
  padding: var(--space-12) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.block-title {
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.block-text {
  margin: var(--space-4) 0 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.state-slot {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
}

.state-reason {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  word-break: break-word;
}

.state-actions {
  display: flex;
  gap: var(--space-8);
}

.actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--space-10);
}

@media (max-width: 860px) {
  .stat-grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (max-width: 720px) {
  .stat-grid {
    grid-template-columns: 1fr 1fr;
  }

  .detail-row {
    grid-template-columns: 1fr;
    gap: 2px;
  }
}
</style>
