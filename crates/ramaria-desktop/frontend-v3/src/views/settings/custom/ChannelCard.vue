<script setup lang="ts">
/**
 * src/views/settings/custom/ChannelCard.vue — 社交平台接入卡
 *
 * 职责:
 * - 在通道配置键之上补充运行态、凭据写入、启停与绑定表
 *
 * 设计特点:
 * - 配置键仍由键表驱动（保存 / 恢复默认与其它卡一致）；本卡只加运行态与即时动作
 * - 平台凭据只写不回显：输入新值即覆盖 keychain，界面只显示「已配置」
 * - 绑定表逐行即时保存；生效人格由服务层计算，保存后整表重读
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseInput from '../../../components/base/BaseInput.vue'
import BaseSelect from '../../../components/base/BaseSelect.vue'
import BaseSwitch from '../../../components/base/BaseSwitch.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { channelApi, configApi, personaApi } from '../../../core/api'
import type { ChannelBinding, ChannelPanel, PersonaFull } from '../../../core/api/dto'
import type { JsonValue } from '../../../config/configTypes'
import type { SettingField, SettingGroup, SettingOption } from '../../../config/schema/define'
import { errorMessage } from '../../../core/errors'
import { pushToast } from '../../../core/toast'
import SchemaGroupCard from '../SchemaGroupCard.vue'

const props = withDefaults(
  defineProps<{
    group: SettingGroup
    document: Record<string, JsonValue>
    dirty?: boolean
    saving?: boolean
    error?: string
    resetDisabled?: boolean
    resetHint?: string
  }>(),
  {
    dirty: false,
    saving: false,
    error: '',
    resetDisabled: false,
    resetHint: '',
  },
)

const emit = defineEmits<{
  update: [field: SettingField, value: JsonValue]
  save: []
  reset: []
}>()

const panel = ref<ChannelPanel | null>(null)
const bindings = ref<ChannelBinding[]>([])
const personas = ref<PersonaFull[]>([])

const statusLoading = ref(false)
const actionKind = ref<'' | 'start' | 'stop'>('')
const bindingLoading = ref('')
const cardError = ref('')
const qqToken = ref('')
const tgToken = ref('')
const savingToken = ref('')

const runningText = computed(() => {
  if (!panel.value) {
    return '读取中…'
  }
  return panel.value.running ? '运行中' : '未运行'
})

const channelStates = computed(() =>
  (panel.value?.channels ?? []).map((item) => `${item.channel} ${item.state}`).join(' · '),
)

const personaOptions = computed<SettingOption[]>(() =>
  personas.value
    .filter((persona) => persona.kind !== 'user')
    .map((persona) => ({ value: persona.uid, label: persona.name })),
)

function personaName(uid: string): string {
  return personas.value.find((persona) => persona.uid === uid)?.name ?? uid
}

async function loadStatus(): Promise<void> {
  if (statusLoading.value) {
    return
  }
  statusLoading.value = true
  try {
    const [nextPanel, nextBindings, nextPersonas] = await Promise.all([
      channelApi.getStatus(),
      channelApi.listBindings(),
      personaApi.listFull(),
    ])
    panel.value = nextPanel
    bindings.value = nextBindings
    personas.value = nextPersonas
  } catch (cause) {
    cardError.value = errorMessage(cause, '通道状态读取失败')
  } finally {
    statusLoading.value = false
  }
}

async function saveToken(platform: 'qq' | 'telegram'): Promise<void> {
  const value = platform === 'qq' ? qqToken.value : tgToken.value
  if (!value.trim() || savingToken.value) {
    return
  }
  savingToken.value = platform
  cardError.value = ''
  try {
    await configApi.updateSetting(
      platform === 'qq' ? 'channel.qq_access_token' : 'channel.telegram_bot_token',
      value.trim(),
    )
    if (platform === 'qq') {
      qqToken.value = ''
    } else {
      tgToken.value = ''
    }
    pushToast({ tone: 'success', message: '凭据已写入系统凭据管理器' })
    await loadStatus()
  } catch (cause) {
    cardError.value = errorMessage(cause, '凭据保存失败')
  } finally {
    savingToken.value = ''
  }
}

async function toggleRuntime(start: boolean): Promise<void> {
  if (actionKind.value) {
    return
  }
  actionKind.value = start ? 'start' : 'stop'
  cardError.value = ''
  try {
    panel.value = start ? await channelApi.start() : await channelApi.stop()
    pushToast({
      tone: panel.value.running === start ? 'success' : 'warn',
      message: panel.value.running === start ? (start ? '通道已启动' : '通道已停止') : '通道未按预期切换，请检查开关与凭据',
    })
  } catch (cause) {
    cardError.value = errorMessage(cause, start ? '启动通道失败' : '停止通道失败')
  } finally {
    actionKind.value = ''
  }
}

async function updateBinding(
  row: ChannelBinding,
  patch: { personaUid?: string; enabled?: boolean; allowProactive?: boolean },
): Promise<void> {
  if (bindingLoading.value) {
    return
  }
  bindingLoading.value = row.externalRef
  cardError.value = ''
  try {
    await channelApi.setBinding(
      row.channel,
      row.externalRef,
      patch.personaUid ?? null,
      patch.enabled ?? null,
      patch.allowProactive ?? null,
    )
    bindings.value = await channelApi.listBindings()
  } catch (cause) {
    cardError.value = errorMessage(cause, '绑定保存失败')
  } finally {
    bindingLoading.value = ''
  }
}

function choosePersona(row: ChannelBinding, value: string): void {
  if (!value || value === row.personaUid) {
    return
  }
  void updateBinding(row, { personaUid: value })
}

onMounted(() => {
  void loadStatus()
})
</script>

<template>
  <SchemaGroupCard
    :group="props.group"
    :document="props.document"
    :dirty="props.dirty"
    :saving="props.saving"
    :error="props.error"
    :reset-disabled="props.resetDisabled"
    :reset-hint="props.resetHint"
    @update="(field, value) => emit('update', field, value)"
    @save="emit('save')"
    @reset="emit('reset')"
  >
    <template #head>
      <BaseTag :tone="panel?.running ? 'ok' : 'neutral'">{{ runningText }}</BaseTag>
      <BaseTag v-if="channelStates" tone="neutral">{{ channelStates }}</BaseTag>
    </template>

    <section class="extra">
      <h4 class="extra-title">平台凭据</h4>
      <div class="token-row">
        <BaseInput
          v-model="qqToken"
          label="QQ Token"
          type="password"
          :placeholder="panel?.qqTokenConfigured ? '已配置，输入新值覆盖' : '未配置'"
          hint="OneBot access token；只写不回显。"
        />
        <BaseButton
          variant="ghost"
          size="sm"
          :loading="savingToken === 'qq'"
          :disabled="!qqToken.trim()"
          @click="saveToken('qq')"
        >
          保存
        </BaseButton>
      </div>
      <div class="token-row">
        <BaseInput
          v-model="tgToken"
          label="Telegram Bot Token"
          type="password"
          :placeholder="panel?.telegramTokenConfigured ? '已配置，输入新值覆盖' : '未配置'"
          hint="BotFather 令牌；只写不回显。"
        />
        <BaseButton
          variant="ghost"
          size="sm"
          :loading="savingToken === 'telegram'"
          :disabled="!tgToken.trim()"
          @click="saveToken('telegram')"
        >
          保存
        </BaseButton>
      </div>
    </section>

    <section class="extra">
      <h4 class="extra-title">外部会话绑定</h4>
      <p v-if="bindings.length === 0" class="extra-empty">
        还没有外部会话；平台收到消息后会自动出现在这里。
      </p>
      <div v-for="row in bindings" :key="`${row.channel}:${row.externalRef}`" class="binding-row">
        <span class="binding-ref" :title="row.externalRef">{{ row.channel }} · {{ row.externalRef }}</span>
        <BaseSelect
          class="binding-persona"
          :model-value="row.personaUid ?? ''"
          label="人格"
          :options="personaOptions"
          :placeholder="`沿用通道默认（${personaName(row.effectivePersonaUid)}）`"
          @update:model-value="choosePersona(row, $event)"
        />
        <BaseSwitch
          :model-value="row.enabled"
          :aria-label="`${row.externalRef} 启用绑定`"
          :disabled="bindingLoading === row.externalRef"
          @update:model-value="updateBinding(row, { enabled: $event })"
        />
        <BaseSwitch
          :model-value="row.allowProactive"
          :aria-label="`${row.externalRef} 允许主动投递`"
          :disabled="bindingLoading === row.externalRef"
          @update:model-value="updateBinding(row, { allowProactive: $event })"
        />
      </div>
      <p class="extra-note">左：启用绑定；右：允许主动投递。</p>
    </section>

    <div v-if="cardError" class="extra-error" role="alert">{{ cardError }}</div>

    <template #footExtra>
      <BaseButton variant="ghost" size="sm" :loading="statusLoading" @click="loadStatus">刷新状态</BaseButton>
      <BaseButton
        variant="ghost"
        size="sm"
        :loading="actionKind === 'stop'"
        :disabled="!!actionKind"
        @click="toggleRuntime(false)"
      >
        停止
      </BaseButton>
      <BaseButton
        variant="secondary"
        size="sm"
        :loading="actionKind === 'start'"
        :disabled="!!actionKind"
        @click="toggleRuntime(true)"
      >
        启动
      </BaseButton>
    </template>
  </SchemaGroupCard>
</template>

<style scoped>
.extra {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  padding-top: var(--space-10);
  border-top: 1px dashed var(--border);
}

.extra-title {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.extra-empty,
.extra-note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.token-row {
  display: flex;
  align-items: flex-end;
  gap: var(--space-8);
}

.token-row :deep(.field) {
  flex: 1;
}

.binding-row {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-6) var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
}

.binding-ref {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.binding-persona {
  flex: 0 0 220px;
}

.extra-error {
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-tag);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}
</style>
