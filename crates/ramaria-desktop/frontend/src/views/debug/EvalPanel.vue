<script setup lang="ts">
/**
 * src/views/debug/EvalPanel.vue — 评估产物页签
 *
 * 职责:
 * - 展示已授权目录、目录内 JSON 产物列表与单个产物的评分摘要
 *
 * 设计特点:
 * - 全程只读：目录由原生对话框授权，选择动作本身即授权
 * - 摘要只取元信息、档位聚合分数与辅助指标；题面、参考答案与模型回复不渲染
 * - 文件行以按钮承载，键盘可达；打开后由「返回文件列表」回到目录视图
 * - 目录未选择与目录为空共用空态，后者补一行说明
 */

import { computed } from 'vue'

import type { EvalFile } from '../../core/api/dto'
import { evalFileMeta, ratioLabel, scoreLabel } from '../../core/debug/format'
import { ARTIFACT_KIND_LABELS } from '../../core/debug/probe'
import type { ArtifactView } from './useDebugView'
import DebugStat from './DebugStat.vue'

const props = defineProps<{
  dir: string
  files: EvalFile[]
  artifact: ArtifactView | null
}>()

const emit = defineEmits<{
  open: [file: EvalFile]
  close: []
}>()

const summary = computed(() => props.artifact?.summary ?? null)

const kindLabel = computed(() =>
  summary.value ? ARTIFACT_KIND_LABELS[summary.value.kind] : '',
)

const judgeLabel = computed(() => {
  const used = summary.value?.meta.judgeUsed
  if (used === null || used === undefined) {
    return '未标注'
  }
  return used ? '已使用' : '未使用'
})

const personaLabel = computed(() => summary.value?.meta.personaUid ?? '未标注')
</script>

<template>
  <div class="eval">
    <template v-if="artifact && summary">
      <div class="artifact-head">
        <button type="button" class="back" @click="emit('close')">
          <svg class="back-icon" viewBox="0 0 12 12" aria-hidden="true">
            <path d="M7.5 2.5L4 6l3.5 3.5" />
          </svg>
          返回文件列表
        </button>
        <h3 class="artifact-title">{{ artifact.name }}</h3>
      </div>

      <div class="stats">
        <DebugStat label="产物类型" :value="kindLabel" />
        <DebugStat label="人格" :value="personaLabel" />
        <DebugStat label="语气判据" :value="judgeLabel" />
        <DebugStat label="档位数" :value="summary.variants.length" />
      </div>

      <p v-if="!summary.hasContent" class="note">
        无法从该文件识别出评估结构，可能是其它 JSON 产物。
      </p>

      <section v-if="summary.auxiliary" class="block">
        <h3 class="block-title">辅助指标</h3>
        <dl class="kv">
          <div v-for="metric in summary.auxiliary" :key="metric.label" class="kv-item">
            <dt class="kv-label">{{ metric.label }}</dt>
            <dd class="kv-value">{{ ratioLabel(metric.value) }}</dd>
          </div>
        </dl>
      </section>

      <section v-if="summary.variants.length > 0" class="block">
        <h3 class="block-title">档位得分</h3>
        <article v-for="variant in summary.variants" :key="variant.name" class="variant">
          <header class="variant-head">
            <code class="variant-name">{{ variant.name }}</code>
            <span v-if="variant.ablation" class="variant-ablation">{{ variant.ablation }}</span>
          </header>
          <p v-if="variant.gateText" class="variant-gate">{{ variant.gateText }}</p>
          <div v-if="variant.scores.length > 0" class="scores">
            <span v-for="score in variant.scores" :key="`${variant.name}-${score.label}`" class="score">
              <b>{{ score.label }}</b>
              <span class="score-value">{{ scoreLabel(score.value) }}</span>
              <em v-if="score.detail">{{ score.detail }}</em>
            </span>
          </div>
          <p v-else class="note">该档位没有可展示的标量得分。</p>
        </article>
      </section>
    </template>

    <template v-else>
      <div class="dir-row">
        <span class="dir-label">产物目录</span>
        <code class="dir-path">{{ dir || '未选择目录' }}</code>
        <span class="dir-count">{{ dir ? `${files.length} 个文件` : '' }}</span>
      </div>

      <ul v-if="files.length > 0" class="file-list">
        <li v-for="file in files" :key="file.path">
          <button type="button" class="file-row" @click="emit('open', file)">
            <span class="file-name">{{ file.name }}</span>
            <span class="file-meta">{{ evalFileMeta(file) }}</span>
            <span class="file-open">查看</span>
          </button>
        </li>
      </ul>
    </template>
  </div>
</template>

<style scoped>
.eval {
  display: flex;
  flex-direction: column;
  gap: var(--space-14);
}

.artifact-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-10) var(--pad-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  box-shadow: var(--shadow-sm);
}

.back {
  display: inline-flex;
  align-items: center;
  gap: var(--space-4);
  padding: 4px var(--space-8);
  border: none;
  border-radius: var(--radius-btn-sm);
  background: transparent;
  color: var(--text-2);
  font-size: var(--fs-small);
  cursor: pointer;
}

.back:hover {
  background: var(--bg-subtle);
  color: var(--brand-text);
}

.back:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.back-icon {
  width: 12px;
  height: 12px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.artifact-title {
  margin: 0;
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  overflow-wrap: anywhere;
}

.stats {
  display: flex;
  gap: var(--gap-card);
}

.block {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  padding: var(--pad-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  box-shadow: var(--shadow-sm);
}

.block-title {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.dir-row {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-10);
  padding: var(--space-10) var(--pad-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  box-shadow: var(--shadow-sm);
}

.dir-label {
  flex: none;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.dir-path {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  overflow-wrap: anywhere;
}

.dir-count {
  flex: none;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.file-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  margin: 0;
  padding: 0;
  list-style: none;
}

.file-row {
  display: flex;
  align-items: baseline;
  gap: var(--space-10);
  width: 100%;
  padding: var(--space-10) var(--pad-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
  color: var(--text-1);
  text-align: left;
  cursor: pointer;
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.file-row:hover {
  border-color: var(--brand-line);
  box-shadow: var(--shadow-xs);
}

.file-row:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.file-name {
  flex: 1;
  min-width: 0;
  font-family: var(--font-mono);
  font-size: var(--fs-small);
  overflow-wrap: anywhere;
}

.file-meta {
  flex: none;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.file-open {
  flex: none;
  color: var(--brand-text);
  font-size: var(--fs-caption);
}

.kv {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: var(--space-6) var(--space-14);
  margin: 0;
}

.kv-item {
  display: flex;
  align-items: baseline;
  gap: var(--space-8);
}

.kv-label {
  flex: none;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.kv-value {
  margin: 0;
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
}

.variant {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.variant-head {
  display: flex;
  align-items: center;
  gap: var(--space-8);
}

.variant-name {
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.variant-ablation {
  padding: 1px var(--space-6);
  border: 1px solid var(--accent-line);
  border-radius: var(--radius-tag);
  background: var(--accent-surface);
  color: var(--accent-text);
  font-size: var(--fs-caption);
}

.variant-gate {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.scores {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-6);
}

.score {
  display: inline-flex;
  align-items: baseline;
  gap: var(--space-4);
  padding: 3px var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-pill);
  background: var(--bg-card);
  font-size: var(--fs-small);
}

.score b {
  color: var(--text-2);
  font-weight: var(--fw-medium);
}

.score-value {
  color: var(--text-1);
  font-family: var(--font-mono);
}

.score em {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-style: normal;
}

@media (max-width: 720px) {
  .stats {
    flex-wrap: wrap;
  }
}
</style>
