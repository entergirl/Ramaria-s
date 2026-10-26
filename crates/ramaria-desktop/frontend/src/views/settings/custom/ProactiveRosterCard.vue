<script setup lang="ts">
/**
 * src/views/settings/custom/ProactiveRosterCard.vue — 主动消息名单卡
 *
 * 职责:
 * - 列出活跃人格的主动开关三态（自动 / 开启 / 关闭），行内即时保存
 *
 * 设计特点:
 * - 生效结论由服务层计算，保存后整表重读，前端不重算
 * - `kind=user` 的人格不参与投递，按只读行展示
 * - 空名单与读取失败都留在卡内，不阻塞其它设置
 */

import { onMounted, ref } from 'vue'

import BaseTag from '../../../components/base/BaseTag.vue'
import { proactiveApi } from '../../../core/api'
import type { ProactivePersonaRow } from '../../../core/api/dto'
import { errorMessage } from '../../../core/errors'
import { pushToast } from '../../../core/toast'
import SettingsCard from '../SettingsCard.vue'

const rows = ref<ProactivePersonaRow[]>([])
const loading = ref(false)
const savingUid = ref('')
const error = ref('')

const MODES: Array<{ value: string; label: string }> = [
  { value: 'auto', label: '自动' },
  { value: 'on', label: '开启' },
  { value: 'off', label: '关闭' },
]

function initial(name: string): string {
  return name.trim().slice(0, 1) || '·'
}

async function load(): Promise<void> {
  if (loading.value) {
    return
  }
  loading.value = true
  error.value = ''
  try {
    rows.value = await proactiveApi.listPersonas()
  } catch (cause) {
    error.value = errorMessage(cause, '主动消息名单读取失败')
  } finally {
    loading.value = false
  }
}

async function chooseMode(row: ProactivePersonaRow, mode: string): Promise<void> {
  if (row.mode === mode || savingUid.value) {
    return
  }
  savingUid.value = row.uid
  try {
    await proactiveApi.setPersona(row.uid, mode)
    await load()
  } catch (cause) {
    pushToast({ tone: 'danger', message: errorMessage(cause, '保存主动开关失败') })
  } finally {
    savingUid.value = ''
  }
}

function modeHint(row: ProactivePersonaRow): string {
  if (row.kind === 'user') {
    return '不参与投递'
  }
  if (!row.has_local_dialogue) {
    return '自动档需先有本地对话'
  }
  return row.effective ? '当前会投递' : '当前不投递'
}

onMounted(() => {
  void load()
})
</script>

<template>
  <SettingsCard
    title="主动消息名单"
    description="自动 = 按判据决定；开启 = 每次调度都投递；关闭 = 不投递。"
    :editable="false"
    :error="error"
  >
    <template #head>
      <BaseTag tone="neutral">{{ rows.length }} 个活跃人格</BaseTag>
    </template>

    <div v-if="rows.length === 0 && !loading" class="empty">
      还没有活跃人格；创建或导入人格后可在这里配置主动开关。
    </div>

    <ul v-else class="roster">
      <li v-for="row in rows" :key="row.uid" class="roster-row" :class="{ 'is-locked': row.kind === 'user' }">
        <span class="roster-avatar" aria-hidden="true">{{ initial(row.name) }}</span>
        <span class="roster-name">{{ row.name }}</span>
        <BaseTag tone="neutral">{{ row.kind }}</BaseTag>
        <span class="roster-spacer" />
        <span class="roster-hint">{{ modeHint(row) }}</span>

        <span v-if="row.kind === 'user'" class="roster-lock">不参与投递</span>
        <span v-else class="tri" role="group" :aria-label="`${row.name} 主动开关`">
          <button
            v-for="mode in MODES"
            :key="mode.value"
            type="button"
            class="tri-btn"
            :class="{ 'is-on': row.mode === mode.value }"
            :disabled="savingUid === row.uid"
            @click="chooseMode(row, mode.value)"
          >
            {{ mode.label }}
          </button>
        </span>
      </li>
    </ul>
  </SettingsCard>
</template>

<style scoped>
.roster {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  margin: 0;
  padding: 0;
  list-style: none;
}

.roster-row {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-6) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
}

.roster-row.is-locked {
  background: var(--bg-sunken);
}

.roster-avatar {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-full);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-size: var(--fs-caption);
}

.roster-name {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
}

.roster-spacer {
  flex: 1;
}

.roster-hint {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.roster-lock {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.tri {
  display: inline-flex;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: var(--radius-btn-sm);
}

.tri-btn {
  height: 24px;
  padding: 0 var(--space-10);
  border: none;
  background: var(--bg-card);
  color: var(--text-2);
  font-size: var(--fs-caption);
  cursor: pointer;
}

.tri-btn + .tri-btn {
  border-left: 1px solid var(--border);
}

.tri-btn.is-on {
  background: var(--btn-bg);
  color: var(--btn-fg);
}

.tri-btn:disabled {
  cursor: default;
  opacity: var(--opacity-disabled);
}

.tri-btn:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.empty {
  padding: var(--space-16);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  color: var(--text-3);
  font-size: var(--fs-small);
  text-align: center;
}
</style>
