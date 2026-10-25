<script setup lang="ts">
/**
 * src/views/persona/index.vue — 人格视图入口
 *
 * 职责:
 * - 编排人格列表 / 详情二级形态与导入、导出、重载确认浮层
 * - 派生列表主态（加载 / 错误 / 空 / 默认）与忙碌标记
 *
 * 设计特点:
 * - 数据与动作在 composable 与 store 中，本组件只做布局与事件接线
 * - 详情为页内二级导航，工具条在两种形态间整体切换，不叠加返回按钮
 * - 未保存修改只在离开详情时拦一次，不阻断页内其它操作
 */
import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import { stateCopy } from '../../core/stateCopy'
import PersonaCard from './PersonaCard.vue'
import PersonaDetailToolbar from './PersonaDetailToolbar.vue'
import PersonaExportModal from './PersonaExportModal.vue'
import PersonaMetaCard from './PersonaMetaCard.vue'
import PersonaProfileCard from './PersonaProfileCard.vue'
import PersonaPromptCard from './PersonaPromptCard.vue'
import PersonaToolbar from './PersonaToolbar.vue'
import PresetImportModal, { type PresetImportPhase } from './PresetImportModal.vue'
import { usePersonaView } from './usePersonaView'

const {
  store,
  saving,
  saveError,
  defaultSaving,
  draft,
  discardOpen,
  reloadOpen,
  reloadingProfile,
  exportOpen,
  exporting,
  exportError,
  importOpen,
  promptOpen,
  promptTier,
  promptInput,
  promptStatic,
  promptFull,
  promptLoading,
  promptError,
  promptHint,
  promptCopied,
  active,
  isDefault,
  defaultCount,
  viewState,
  busy,
  partialError,
  dirty,
  canSave,
  llmUnavailable,
  profileDegraded,
  profileChip,
  profileNote,
  init,
  openDetail,
  requestBack,
  confirmDiscard,
  setName,
  setAvatar,
  setDescription,
  save,
  setDefault,
  requestReload,
  reloadProfile,
  openExport,
  confirmExport,
  openImport,
  requestPrompt,
  togglePrompt,
  setPromptTier,
  setPromptInput,
  copyPrompt,
  onImported,
  retry,
} = usePersonaView()

const emptyCopy = stateCopy('persona.empty.no-data')
const errorCopy = stateCopy('persona.error.load')

const emptyTitle = computed(() => emptyCopy.title)
const emptyDescription = computed(() => emptyCopy.description)
const emptyAction = computed(() => emptyCopy.action ?? '导入预设')

const importPhase = ref<PresetImportPhase>('idle')

/** 降级标记：画像未生成只在详情形态生效；对话模型不可用为全局能力 */
const degradedAttr = computed(() => {
  const parts: string[] = []
  if (store.pane === 'detail' && profileDegraded.value) {
    parts.push('profile')
  }
  if (llmUnavailable.value) {
    parts.push('llm')
  }
  return parts.length > 0 ? parts.join(' ') : undefined
})

/** 受限标记：预设写入中锁流程；预览为只读 */
const restrictedAttr = computed<'locked' | 'readonly' | undefined>(() => {
  if (importPhase.value === 'importing') {
    return 'locked'
  }
  return importPhase.value === 'preview' ? 'readonly' : undefined
})

onMounted(() => {
  void init()
})
</script>

<template>
  <BaseWorkbench>
    <div
      class="persona"
      :data-view-state="store.pane === 'list' ? viewState : 'default'"
      :data-degraded="degradedAttr"
      :data-restricted="restrictedAttr"
      :aria-busy="busy || saving ? 'true' : undefined"
    >
      <template v-if="store.pane === 'list'">
        <PersonaToolbar
          :count="store.personas.length"
          :default-count="defaultCount"
          :state="viewState"
          :busy="busy"
          @import="openImport"
        />

        <div v-if="partialError" class="error-strip" role="alert">
          <span class="error-strip-text">{{ partialError }}</span>
          <BaseButton variant="ghost" size="sm" @click="retry">重试</BaseButton>
        </div>

        <div v-if="viewState === 'loading'" class="grid">
          <BaseCard v-for="index in 4" :key="index" class="sk-card">
            <BaseSkeleton shape="card" :lines="3" />
          </BaseCard>
        </div>

        <BaseEmptyState
          v-else-if="viewState === 'error'"
          variant="blocked"
          :title="errorCopy.title"
          :description="errorCopy.description"
        >
          <div class="state-slot">
            <p class="state-reason">{{ store.listError }}</p>
            <BaseButton variant="secondary" size="sm" @click="retry">重试</BaseButton>
          </div>
        </BaseEmptyState>

        <BaseEmptyState
          v-else-if="viewState === 'empty'"
          variant="action"
          :title="emptyTitle"
          :description="emptyDescription"
        >
          <BaseButton variant="secondary" size="sm" @click="openImport">
            {{ emptyAction }}
          </BaseButton>
        </BaseEmptyState>

        <div v-else class="grid">
          <PersonaCard
            v-for="persona in store.personas"
            :key="persona.uid"
            :persona="persona"
            :is-default="persona.uid === store.defaultUid"
            @open="openDetail(persona.uid)"
          />
        </div>
      </template>

      <template v-else-if="active && draft">
        <PersonaDetailToolbar
          :persona="active"
          :name="draft.name"
          :avatar="draft.avatar"
          :is-default="isDefault"
          :dirty="dirty"
          :saving="saving"
          :can-save="canSave"
          @back="requestBack"
          @save="save"
        />

        <div v-if="saveError" class="error-strip" role="alert">
          <span class="error-strip-text">{{ saveError }}</span>
        </div>

        <div class="detail-grid">
          <PersonaProfileCard
            :name="draft.name"
            :avatar="draft.avatar"
            :description="draft.description"
            :kind="active.kind"
            :compact="promptOpen"
            @update:name="setName"
            @update:avatar="setAvatar"
            @update:description="setDescription"
          />

          <PersonaMetaCard
            :persona="active"
            :is-default="isDefault"
            :default-saving="defaultSaving"
            :default-error="store.defaultError"
            :profile-chip="profileChip"
            :profile-note="profileNote"
            :profile-degraded="profileDegraded"
            :reloading="reloadingProfile"
            :llm-unavailable="llmUnavailable"
            :compact="promptOpen"
            @set-default="setDefault"
            @reload="requestReload"
            @export="openExport"
          />

          <PersonaPromptCard
            :open="promptOpen"
            :tier="promptTier"
            :input="promptInput"
            :static-preview="promptStatic"
            :full-preview="promptFull"
            :loading="promptLoading"
            :error="promptError"
            :hint="promptHint"
            :copied="promptCopied"
            @toggle="togglePrompt"
            @update:tier="setPromptTier"
            @update:input="setPromptInput"
            @request="requestPrompt"
            @copy="copyPrompt"
          />
        </div>
      </template>
    </div>
  </BaseWorkbench>

  <PresetImportModal
    :open="importOpen"
    @close="importOpen = false"
    @imported="onImported"
    @phase="importPhase = $event"
  />

  <PersonaExportModal
    :open="exportOpen"
    :persona="active"
    :busy="exporting"
    :error="exportError"
    @close="exportOpen = false"
    @confirm="confirmExport"
  />

  <BaseModal
    :open="discardOpen"
    title="放弃未保存的修改？"
    @close="discardOpen = false"
  >
    <p class="modal-copy">
      该人格的名称、头像或描述还没有保存。返回列表会丢失这次修改。
    </p>
    <template #foot>
      <span class="spacer" />
      <BaseButton variant="ghost" @click="discardOpen = false">继续编辑</BaseButton>
      <BaseButton @click="confirmDiscard">放弃并返回</BaseButton>
    </template>
  </BaseModal>

  <BaseModal
    :open="reloadOpen"
    title="重载性格画像"
    :loading="reloadingProfile"
    @close="reloadOpen = false"
  >
    <p v-if="active" class="modal-copy">
      将按最新 L2 事件重算「{{ active.name }}」的 L3 画像。重载可能需要几秒，期间不影响对话。
    </p>
    <p class="modal-note">当前状态：{{ profileChip.text }}</p>
    <template #foot>
      <span class="spacer" />
      <BaseButton variant="ghost" :disabled="reloadingProfile" @click="reloadOpen = false">
        取消
      </BaseButton>
      <BaseButton :loading="reloadingProfile" @click="reloadProfile">开始重载</BaseButton>
    </template>
  </BaseModal>
</template>

<style scoped>
.persona {
  display: flex;
  flex-direction: column;
  gap: var(--gap-card);
  width: 100%;
  max-width: var(--w-workbench);
  margin: 0 auto;
}

.grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--gap-card);
}

.sk-card {
  min-height: 156px;
  padding: var(--pad-card);
}

.detail-grid {
  display: grid;
  grid-template-columns: minmax(0, 7fr) minmax(0, 5fr);
  gap: var(--space-10);
  align-items: stretch;
}

.error-strip {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-inner);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}

.error-strip-text {
  flex: 1;
  min-width: 0;
}

.state-slot {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-8);
}

.state-reason {
  max-width: 42ch;
  margin: 0;
  color: var(--danger);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.modal-copy {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-body);
  line-height: 1.7;
}

.modal-note {
  margin: var(--space-8) 0 0;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.spacer {
  flex: 1;
}

@media (max-width: 860px) {
  .grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .detail-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
