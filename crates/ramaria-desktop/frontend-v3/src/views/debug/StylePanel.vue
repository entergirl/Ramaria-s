<script setup lang="ts">
/**
 * src/views/debug/StylePanel.vue — 说话风格页签
 *
 * 职责:
 * - 按人格展示样本量、统计状态、规则来源、自动规则文本与五维统计参数
 *
 * 设计特点:
 * - 只读：统计由封存钩子在 L1 阶段触发，本页不提供重算入口
 * - 五维按口癖词 / 句式句长 / 标点 / 情感表达 / 话题词汇固定顺序分组
 * - 计数字段附每千字频率；词频类条目以芯片呈现，已在格式化层截断
 * - 状态由后端给中文标签，色调只区分「可用 / 样本不足 / 无显著项」
 */

import { computed } from 'vue'

import BaseSelect from '../../components/base/BaseSelect.vue'
import type { PersonaSummary } from '../../core/api/dto'
import type { StyleStatsModel } from '../../core/debug/format'
import DebugStat from './DebugStat.vue'

const props = defineProps<{
  personas: PersonaSummary[]
  personaUid: string | null
  model: StyleStatsModel | null
}>()

const emit = defineEmits<{
  'select-persona': [uid: string]
}>()

const options = computed(() =>
  props.personas.map((persona) => ({ value: persona.uid, label: persona.name || persona.uid })),
)

const readonlyValue = computed(() => props.personaUid ?? '')

function onSelect(uid: string): void {
  emit('select-persona', uid)
}
</script>

<template>
  <div class="style">
    <div class="style-head">
      <BaseSelect
        class="style-persona"
        label="人格"
        :model-value="readonlyValue"
        :options="options"
        @update:model-value="onSelect"
      />
      <span v-if="model" class="style-meta">统计更新于 {{ model.updatedAt }}</span>
    </div>

    <template v-if="model">
      <div class="stats">
        <DebugStat label="样本量 n_p" :value="model.sampleCount" />
        <DebugStat
          label="统计状态"
          :value="model.statusLabel"
          :tone="model.statusTone"
          :note="model.statusHint"
        />
        <DebugStat label="规则来源" :value="model.ruleSourceLabel" tone="accent" />
        <DebugStat label="基线版本" :value="model.baselineVersion" />
      </div>

      <section class="block">
        <h3 class="block-title">自动规则文本</h3>
        <p v-if="model.ruleText" class="rule-text">{{ model.ruleText }}</p>
        <p v-else class="note">未生成规则文本（数据不足、无显著项或风格通道关闭）。</p>
      </section>

      <section v-for="group in model.groups" :key="group.id" class="block">
        <h3 class="block-title">{{ group.title }}</h3>

        <div v-if="group.chips.length > 0" class="chips">
          <span v-for="chip in group.chips" :key="`${group.id}-${chip.text}`" class="chip">
            {{ chip.text }}<em>×{{ chip.count }}</em>
          </span>
        </div>

        <dl v-if="group.fields.length > 0" class="kv">
          <div v-for="field in group.fields" :key="`${group.id}-${field.label}`" class="kv-item">
            <dt class="kv-label">{{ field.label }}</dt>
            <dd class="kv-value">{{ field.value }}</dd>
          </div>
        </dl>
      </section>

      <p v-if="!model.parsable" class="note">统计参数无法解析，仅显示样本与规则信息。</p>
      <p v-else-if="model.groups.length === 0" class="note">统计参数为空，等待下一次封存累积。</p>
    </template>
  </div>
</template>

<style scoped>
.style {
  display: flex;
  flex-direction: column;
  gap: var(--space-14);
}

.style-head {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: var(--space-14);
  padding: var(--space-12) var(--pad-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  box-shadow: var(--shadow-sm);
}

.style-persona {
  width: 260px;
}

.style-meta {
  margin-left: auto;
  color: var(--text-3);
  font-size: var(--fs-caption);
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

.rule-text {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-small);
  line-height: 1.8;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-6);
}

.chip {
  display: inline-flex;
  align-items: baseline;
  gap: var(--space-4);
  padding: 3px var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-pill);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-size: var(--fs-small);
}

.chip em {
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  font-style: normal;
}

.kv {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
  gap: var(--space-6) var(--space-14);
  margin: 0;
}

.kv-item {
  display: flex;
  align-items: baseline;
  gap: var(--space-8);
  min-width: 0;
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
  overflow-wrap: anywhere;
}

@media (max-width: 720px) {
  .stats {
    flex-wrap: wrap;
  }

  .style-persona {
    width: 100%;
  }
}
</style>
