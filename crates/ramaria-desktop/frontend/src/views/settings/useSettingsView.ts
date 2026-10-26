/**
 * src/views/settings/useSettingsView.ts — 设置页的加载、保存与回滚编排
 *
 * 职责:
 * - 读取生效配置与出厂默认配置，维护草稿 / 基线 / 脏值与单卡保存状态
 * - 提供按组保存、按组恢复默认、单卡错误与重试入口
 *
 * 设计特点:
 * - 保存前重新读取最新配置再合并本组字段，避免用页面打开时的旧快照覆盖他处改动
 * - 恢复默认只改表单并标脏（需再点保存），不做不可逆的即时写入
 * - 单侧写入失败按「已生效 + 警告」处理；双侧失败留在卡内错误条
 * - 自定义卡（隐私 / 概览 / 名单 / 数据 / 诊断）不参与草稿，即时保存
 */

import { computed, ref } from 'vue'

import { configApi } from '../../core/api'
import {
  applyDefaults,
  cloneDocument,
  dirtyFieldKeys,
  mergeFields,
  readPath,
  snapshotFields,
  writePath,
} from '../../config/configDraft'
import {
  type ConfigDocument,
  parseConfigDocument,
  stringifyConfigDocument,
} from '../../config/configTypes'
import {
  type SettingsSection,
  defaultSectionId,
  findSection,
  isGroupCard,
  sectionsOf,
} from '../../config/settingsSections'
import { fieldsOfGroup } from '../../config/settingsSchema'
import type { JsonValue } from '../../config/configTypes'
import type { SettingField } from '../../config/schema/define'
import { errorMessage } from '../../core/errors'
import { pushToast } from '../../core/toast'
import { useSettingsStore } from '../../stores/settings'

/** 视图主态（与设计规格六态对齐的机器口径） */
export type SettingsViewState = 'loading' | 'error' | 'default'

export function useSettingsView() {
  const store = useSettingsStore()

  const baseline = ref<ConfigDocument | null>(null)
  const draft = ref<ConfigDocument | null>(null)
  const defaults = ref<ConfigDocument | null>(null)
  const loading = ref(false)
  const loadError = ref('')
  const defaultsError = ref('')
  const savingCard = ref<string | null>(null)
  const cardErrors = ref<Record<string, string>>({})

  const ready = computed(() => draft.value !== null && baseline.value !== null)
  const viewState = computed<SettingsViewState>(() => {
    if (loadError.value) {
      return 'error'
    }
    return ready.value ? 'default' : 'loading'
  })

  const scope = computed(() => store.scope)
  const sectionId = computed(() => store.sectionId)
  const sections = computed<SettingsSection[]>(() => sectionsOf(scope.value))
  const section = computed<SettingsSection>(() => {
    const fallbackId = defaultSectionId(scope.value)
    return (
      findSection(scope.value, sectionId.value) ??
      findSection(scope.value, fallbackId) ?? {
        id: fallbackId,
        label: '',
        cards: [],
      }
    )
  })

  /** 各子页内与基线不同的字段数（键为子页 id，供目录键徽标使用） */
  const dirtyBySection = computed<Record<string, number>>(() => {
    const counts: Record<string, number> = {}
    if (!draft.value || !baseline.value) {
      return counts
    }
    for (const item of sections.value) {
      let count = 0
      for (const card of item.cards) {
        if (!isGroupCard(card.kind)) {
          continue
        }
        count += groupDirtyKeys(card.id).length
      }
      counts[item.id] = count
    }
    return counts
  })

  function groupFields(groupId: string): SettingField[] {
    return fieldsOfGroup(groupId)
  }

  function groupDirtyKeys(groupId: string): string[] {
    if (!draft.value || !baseline.value) {
      return []
    }
    const fields = groupFields(groupId)
    return dirtyFieldKeys(draft.value, snapshotFields(baseline.value, fields), fields)
  }

  function isGroupDirty(groupId: string): boolean {
    return groupDirtyKeys(groupId).length > 0
  }

  function fieldValue(field: SettingField): JsonValue | undefined {
    return draft.value ? readPath(draft.value, field.path) : undefined
  }

  function setField(field: SettingField, value: JsonValue): void {
    if (!draft.value) {
      return
    }
    draft.value = writePath(draft.value, field.path, value)
  }

  /** 读取生效配置与默认配置；`force` 用于错误重试 */
  async function load(force = false): Promise<void> {
    if (loading.value || (ready.value && !force)) {
      return
    }
    loading.value = true
    loadError.value = ''
    try {
      const [fullText, defaultText] = await Promise.all([
        configApi.getFull(),
        configApi.getDefaults().catch((error: unknown) => {
          defaultsError.value = errorMessage(error, '默认配置读取失败')
          return ''
        }),
      ])
      const full = parseConfigDocument(fullText)
      baseline.value = cloneDocument(full)
      draft.value = cloneDocument(full)
      if (defaultText) {
        defaults.value = parseConfigDocument(defaultText)
        defaultsError.value = ''
      }
    } catch (error) {
      loadError.value = errorMessage(error, '设置加载失败')
    } finally {
      loading.value = false
    }
  }

  function clearCardError(groupId: string): void {
    if (!cardErrors.value[groupId]) {
      return
    }
    const next = { ...cardErrors.value }
    delete next[groupId]
    cardErrors.value = next
  }

  function setCardError(groupId: string, message: string): void {
    cardErrors.value = { ...cardErrors.value, [groupId]: message }
  }

  /**
   * 保存一个配置组：读最新 → 只合入本组 → 全量写回。
   *
   * 返回:
   * - `true`: 至少一侧写入成功（表单基线同步）；`false`: 双侧失败（错误留在卡内）
   */
  async function saveGroup(groupId: string): Promise<boolean> {
    if (!draft.value || savingCard.value) {
      return false
    }
    const fields = groupFields(groupId)
    if (fields.length === 0) {
      return false
    }

    savingCard.value = groupId
    clearCardError(groupId)
    try {
      const latest = parseConfigDocument(await configApi.getFull())
      const merged = mergeFields(latest, draft.value, fields)
      const receipt = await configApi.updateFull(stringifyConfigDocument(merged))

      if (!receipt.fileOk && !receipt.dbOk) {
        const reason = receipt.failures[0] ?? '配置文件与数据库均未写入'
        setCardError(groupId, reason)
        pushToast({ tone: 'danger', message: `保存失败：${reason}` })
        return false
      }

      baseline.value = cloneDocument(merged)
      if (!receipt.fileOk || !receipt.dbOk) {
        const reason = receipt.failures[0] ?? (receipt.fileOk ? '数据库侧未写入' : '配置文件未写入')
        pushToast({ tone: 'warn', message: `已保存，但有一侧未同步：${reason}` })
      } else {
        pushToast({ tone: 'success', message: '设置已保存' })
      }
      return true
    } catch (error) {
      const message = errorMessage(error, '保存设置失败')
      setCardError(groupId, message)
      pushToast({ tone: 'danger', message })
      return false
    } finally {
      savingCard.value = null
    }
  }

  /** 恢复默认：只把本组字段填为默认值并标脏，需再点保存 */
  function resetGroup(groupId: string): void {
    if (!draft.value) {
      return
    }
    if (!defaults.value) {
      pushToast({ tone: 'warn', message: defaultsError.value || '默认配置不可用，暂不能恢复默认' })
      return
    }
    const fields = groupFields(groupId)
    draft.value = applyDefaults(draft.value, defaults.value, fields)
    clearCardError(groupId)
    pushToast({ tone: 'info', message: '已填入默认值，保存后生效' })
  }

  function applyRoute(nextScope: 'basic' | 'advanced', nextSection: string): void {
    store.goTo(nextScope, nextSection)
  }

  return {
    baseline,
    draft,
    defaults,
    loading,
    loadError,
    defaultsError,
    savingCard,
    cardErrors,
    ready,
    viewState,
    scope,
    sectionId,
    sections,
    section,
    dirtyBySection,
    groupFields,
    groupDirtyKeys,
    isGroupDirty,
    fieldValue,
    setField,
    load,
    saveGroup,
    resetGroup,
    clearCardError,
    applyRoute,
  }
}
