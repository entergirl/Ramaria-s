<script setup lang="ts">
/**
 * src/views/import/ImportDoneStep.vue — 导入向导第四步（完成）
 *
 * 职责:
 * - 展示导入统计、L1 失败与快速模式提示、续跑统计
 * - 提供「导入另一个文件」「查看导入消息」「前往记忆页面」「继续处理」
 *
 * 设计特点:
 * - 失败与降级用文案 + 图标位表达，不只靠颜色
 * - 一屏只保留一个主按钮（优先「前往记忆页面」）
 * - 续跑完成没有导入结果，只展示续跑统计
 */

import BaseButton from '../../components/base/BaseButton.vue'
import type { ImportResult } from '../../core/api/dto'
import type { ImportMetric } from '../../core/import/progress'

defineProps<{
  continueJob: boolean
  result: ImportResult | null
  metrics: ImportMetric[]
  l1Failed: number
  l1Total: number
  visionFailed: number
  skippedAttachments: number
  members: string
  timeRange: string
  canChat: boolean
  canMemory: boolean
}>()

const emit = defineEmits<{
  another: []
  chat: []
  memory: []
  continueImport: []
}>()
</script>

<template>
  <div class="step-body">
    <header class="done-head">
      <div>
        <div class="done-title">{{ continueJob ? '继续处理完成' : '导入完成' }}</div>
        <div class="done-sub">
          {{ continueJob ? '附件与摘要已按最新状态补齐' : '对话已写入本地库；记忆管线在后台继续' }}
        </div>
      </div>
      <span
        class="done-badge"
        :class="{ 'is-warn': l1Failed > 0 || visionFailed > 0 || skippedAttachments > 0 }"
      >
        {{ l1Failed > 0 || visionFailed > 0 || skippedAttachments > 0 ? '有未完成项' : '已完成' }}
      </span>
    </header>

    <div v-if="l1Failed > 0" class="notice notice--warn" role="status">
      <div class="notice-title">L1 摘要生成失败 {{ l1Failed }} / {{ l1Total }} 条</div>
      <p class="notice-text">
        通常是对话模型暂不可用。确认模型连接后，可在记忆页选择对应人格，点击「深度处理导入的消息」重新生成。
      </p>
    </div>

    <div v-if="visionFailed > 0" class="notice notice--warn" role="status">
      <div class="notice-title">有 {{ visionFailed }} 张图片未完成理解</div>
      <p class="notice-text">补齐导出目录后再次「继续处理」，可按 md5 去重回补。</p>
    </div>

    <div v-if="skippedAttachments > 0" class="notice notice--warn" role="status">
      <div class="notice-title">有 {{ skippedAttachments }} 个附件此前被跳过</div>
      <p class="notice-text">选择包含 resources/ 的导出目录后「继续处理」，可重新定位并理解。</p>
    </div>

    <div v-if="!continueJob && result?.mode === 'fast'" class="notice" role="status">
      <div class="notice-title">快速导入未生成记忆摘要</div>
      <p class="notice-text">
        如需摘要、事件与画像，可稍后点击「继续处理」补跑，或前往记忆页手动触发生成。
      </p>
    </div>

    <div v-if="metrics.length" class="stat-grid">
      <div v-for="item in metrics" :key="item.key" class="stat">
        <div class="stat-label">{{ item.label }}</div>
        <div class="stat-value">{{ item.value }}</div>
      </div>
    </div>

    <dl v-if="result" class="detail-list">
      <div class="detail-row">
        <dt>人格</dt>
        <dd>
          {{ result.persona_name || '未创建' }}
          <span v-if="result.other_persona_name" class="muted">
            · 对方 {{ result.other_persona_name }}
          </span>
        </dd>
      </div>
      <div class="detail-row">
        <dt>对话</dt>
        <dd>{{ result.chat_name || '未知' }}</dd>
      </div>
      <div class="detail-row">
        <dt>时间范围</dt>
        <dd>{{ timeRange }}</dd>
      </div>
    </dl>

    <div v-if="members" class="group-block">
      <div class="block-title">群聊成员（按消息数）</div>
      <p class="block-text">{{ members }}</p>
    </div>

    <div class="actions">
      <BaseButton v-if="canMemory" @click="emit('memory')">前往记忆页面</BaseButton>
      <BaseButton v-else @click="emit('another')">导入另一个文件</BaseButton>
      <BaseButton v-if="canChat" variant="secondary" @click="emit('chat')">查看导入消息</BaseButton>
      <BaseButton variant="secondary" @click="emit('continueImport')">继续处理</BaseButton>
      <span class="spacer" />
      <BaseButton v-if="canMemory" variant="ghost" @click="emit('another')">
        导入另一个文件
      </BaseButton>
    </div>
  </div>
</template>

<style scoped>
.step-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-16);
}

.done-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-12);
}

.done-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.done-sub {
  margin-top: 2px;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.done-badge {
  flex: none;
  padding: 2px 8px;
  border-radius: var(--radius-tag);
  background: var(--ok-surface);
  color: var(--ok);
  font-size: var(--fs-caption);
}

.done-badge.is-warn {
  background: var(--warn-surface);
  color: var(--warn);
}

.notice {
  padding: var(--space-12) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.notice--warn {
  border-color: var(--warn-line);
  background: var(--warn-surface);
}

.notice-title {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.notice-text {
  margin: var(--space-4) 0 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.stat-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
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

.detail-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  margin: 0;
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

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-10);
}

.spacer {
  flex: 1;
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
