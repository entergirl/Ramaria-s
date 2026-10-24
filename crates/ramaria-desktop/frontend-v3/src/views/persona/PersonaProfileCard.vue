<script setup lang="ts">
/**
 * src/views/persona/PersonaProfileCard.vue — 基础资料卡
 *
 * 职责:
 * - 选取本地图片作为头像，编辑名称与描述
 * - 提示词展开时压缩为单行摘要（头像 + 当前名称）
 *
 * 设计特点:
 * - 头像以本地文件为来源：选图后裁成方形并压成 data URI 写入草稿，保存前不落库
 * - 头像即按钮，副键「选择图片」同入口，避免只能靠猜的可点区域
 * - 描述用多行输入并在输入处截断，提示行固定占位不改变卡高
 * - 压缩态只留身份摘要，字段整体隐藏，避免展开提示词时来回滚动
 */
import { computed, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'
import { personaInitial } from '../../core/memory/format'
import {
  AVATAR_ACCEPT,
  avatarFileError,
  fileToAvatarDataUrl,
} from '../../core/persona/avatar'
import { DESCRIPTION_MAX, nameError, NAME_MAX } from '../../core/persona/draft'

const props = defineProps<{
  name: string
  avatar: string
  description: string
  kind: string
  compact: boolean
}>()

const emit = defineEmits<{
  'update:name': [value: string]
  'update:avatar': [value: string]
  'update:description': [value: string]
}>()

const fileInput = ref<HTMLInputElement | null>(null)
const picking = ref(false)
const avatarError = ref('')
const previewFailed = ref(false)

const initial = computed(() => personaInitial(props.name))
const avatarSrc = computed(() => {
  const value = props.avatar.trim()
  return value.length > 0 && !previewFailed.value ? value : ''
})
const error = computed(() => nameError(props.name))
const descLength = computed(() => props.description.length)
const avatarHint = computed(() => {
  if (avatarError.value !== '') {
    return avatarError.value
  }
  if (picking.value) {
    return '正在处理图片…'
  }
  return '点击头像选择本地图片，自动裁成方形并压缩。'
})

watch(
  () => props.avatar,
  () => {
    previewFailed.value = false
  },
)

function openPicker(): void {
  if (props.compact || picking.value) {
    return
  }
  avatarError.value = ''
  fileInput.value?.click()
}

async function onFile(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file) {
    return
  }
  const invalid = avatarFileError(file)
  if (invalid !== '') {
    avatarError.value = invalid
    return
  }
  picking.value = true
  try {
    emit('update:avatar', await fileToAvatarDataUrl(file))
  } catch {
    avatarError.value = '图片读取失败，请换一张'
  } finally {
    picking.value = false
  }
}

function clearAvatar(): void {
  avatarError.value = ''
  emit('update:avatar', '')
}

function onNameInput(event: Event): void {
  emit('update:name', (event.target as HTMLInputElement).value)
}

function onDescriptionInput(event: Event): void {
  emit('update:description', (event.target as HTMLTextAreaElement).value)
}
</script>

<template>
  <BaseCard class="profile-card" :class="{ 'is-compact': compact }">
    <header class="section-head">
      <div class="head-text">
        <div class="section-title">基础资料</div>
        <div class="section-sub">名称、描述与头像会进入该人格的展示与提示词。</div>
      </div>
    </header>

    <div class="avatar-row">
      <button
        type="button"
        class="avatar-btn"
        :disabled="compact || picking"
        aria-label="选择头像图片"
        @click="openPicker"
      >
        <span class="avatar" aria-hidden="true">
          <img v-if="avatarSrc" :src="avatarSrc" alt="" @error="previewFailed = true" />
          <template v-else>{{ initial }}</template>
        </span>
        <span class="avatar-mask" aria-hidden="true">{{ picking ? '…' : '更换' }}</span>
      </button>

      <div class="avatar-side">
        <div class="avatar-actions">
          <BaseButton size="sm" variant="secondary" :loading="picking" @click="openPicker">
            选择图片
          </BaseButton>
          <BaseButton v-if="avatar.trim() !== ''" size="sm" variant="ghost" @click="clearAvatar">
            清除
          </BaseButton>
        </div>
        <p class="field-hint" :class="{ 'is-error': avatarError !== '' }">{{ avatarHint }}</p>
      </div>

      <span class="compact-name">{{ name.trim() || '未命名' }}</span>
      <input
        ref="fileInput"
        class="file-input"
        type="file"
        :accept="AVATAR_ACCEPT"
        tabindex="-1"
        aria-hidden="true"
        @change="onFile"
      />
    </div>

    <div class="field">
      <label class="field-label" for="persona-name-input">
        名称 <span class="req">必填</span>
      </label>
      <input
        id="persona-name-input"
        class="input"
        type="text"
        autocomplete="off"
        :value="name"
        @input="onNameInput"
      />
      <p class="field-hint" :class="{ 'is-error': error !== '' }">
        {{ error || `用于称呼与提示词；最多 ${NAME_MAX} 字。` }}
      </p>
    </div>

    <div class="field field--desc">
      <label class="field-label" for="persona-desc-input">描述</label>
      <textarea
        id="persona-desc-input"
        class="input textarea"
        rows="2"
        :value="description"
        @input="onDescriptionInput"
      />
      <p class="field-hint">{{ descLength }} / {{ DESCRIPTION_MAX }}；描述会参与人格设定。</p>
    </div>

    <div class="form-foot">
      <span class="type-inline">类型 · <b>{{ kind }}</b></span>
      <span class="spacer" />
      <span class="form-note">修改后需保存</span>
    </div>
  </BaseCard>
</template>

<style scoped>
.profile-card {
  gap: var(--space-8);
  padding: var(--pad-card);
}

.section-head {
  display: flex;
  align-items: flex-start;
  gap: var(--space-8);
}

.head-text {
  min-width: 0;
}

.section-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
}

.section-sub {
  margin-top: 2px;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.5;
}

.avatar-row {
  display: flex;
  align-items: center;
  gap: var(--space-12);
}

.avatar-btn {
  position: relative;
  flex: none;
  width: 44px;
  height: 44px;
  padding: 0;
  border: none;
  border-radius: var(--radius-full);
  background: transparent;
  cursor: pointer;
}

.avatar-btn:disabled {
  cursor: default;
}

.avatar {
  display: grid;
  place-items: center;
  width: 100%;
  height: 100%;
  overflow: hidden;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  box-shadow: inset 0 0 0 1px var(--brand-line);
  color: var(--brand-text);
  font-size: var(--fs-h2);
  font-weight: var(--fw-bold);
  line-height: 1;
}

.avatar img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.avatar-mask {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  border-radius: var(--radius-full);
  background: rgb(0 0 0 / 45%);
  color: #fff;
  font-size: var(--fs-caption);
  opacity: 0;
  transition: opacity var(--dur-fast) var(--ease-out);
}

.avatar-btn:hover .avatar-mask,
.avatar-btn:focus-visible .avatar-mask,
.avatar-btn:disabled .avatar-mask {
  opacity: 1;
}

.avatar-btn:disabled .avatar-mask {
  display: none;
}

.avatar-btn:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.avatar-side {
  flex: 1;
  min-width: 0;
}

.avatar-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.file-input {
  display: none;
}

.compact-name {
  display: none;
  min-width: 0;
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-bold);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.field-label {
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
}

.req {
  color: var(--accent-text);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}

.input {
  width: 100%;
  height: 32px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-family: inherit;
  font-size: var(--fs-body);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.input:hover {
  border-color: var(--brand-line);
}

.input:focus-visible {
  outline: none;
  border-color: var(--brand);
  box-shadow: var(--focus-ring);
}

/* 描述吃掉卡片剩余高度，避免与右侧卡等高后留白 */
.field--desc {
  flex: 1;
  min-height: 0;
}

.textarea {
  flex: 1;
  height: auto;
  min-height: 56px;
  padding: var(--space-6) var(--space-10);
  line-height: 1.55;
  resize: vertical;
}

.field-hint {
  min-height: 17px;
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.45;
}

.avatar-side .field-hint {
  margin-top: var(--space-4);
}

.field-hint.is-error {
  color: var(--danger);
}

.form-foot {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.type-inline {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.type-inline b {
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.form-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.spacer {
  flex: 1;
}

/* ===== 压缩态：只留身份摘要 ===== */
.is-compact {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
  height: 52px;
  padding: var(--space-8) var(--space-16);
  gap: var(--space-10);
}

.is-compact .section-head {
  flex: 1;
  min-width: 0;
}

.is-compact .section-sub {
  display: none;
}

.is-compact .avatar-row {
  flex: none;
  align-items: center;
  gap: var(--space-8);
}

.is-compact .avatar-btn {
  width: 32px;
  height: 32px;
}

.is-compact .avatar-mask,
.is-compact .avatar-side,
.is-compact .field,
.is-compact .form-foot {
  display: none;
}

.is-compact .compact-name {
  display: block;
}

@media (max-width: 720px) {
  .avatar-row {
    flex-wrap: wrap;
  }
}
</style>
