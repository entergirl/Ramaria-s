<script setup lang="ts">
/**
 * src/spike/SpikePage.vue — 工具链冒烟页
 *
 * 职责:
 * - 验证 IPC 链路（一条文本命令）与二进制链路（命令返回原始字节并渲染为图片）
 * - 显示运行时可用性与失败原因，供首次启动与开发期自检
 *
 * 设计特点:
 * - 只依赖传输层，不引业务 store，属一次性冒烟页面
 * - 图片经 Blob URL 渲染，替换与卸载时立即回收，避免常驻内存
 * - 错误文案直接取自传输层，不在此处重写语气
 */

import { onBeforeUnmount, onMounted, ref } from 'vue'

import BaseButton from '../components/base/BaseButton.vue'
import BaseWorkbench from '../components/base/BaseWorkbench.vue'
import { attachmentApi, systemApi } from '../core/api'
import { currentTheme, toggleTheme } from '../core/theme'
import { isTauriAvailable, TransportError } from '../core/transport'

const runtimeReady = ref(isTauriAvailable())
const version = ref('')
const versionError = ref('')
const theme = ref(currentTheme())

const messageId = ref('')
const imageIndex = ref(0)
const imageUrl = ref('')
const imageError = ref('')
const loadingImage = ref(false)

async function loadVersion(): Promise<void> {
  versionError.value = ''
  try {
    version.value = await systemApi.getVersion()
  } catch (error) {
    versionError.value = readable(error)
  }
}

function switchTheme(): void {
  theme.value = toggleTheme()
}

async function loadImage(): Promise<void> {
  imageError.value = ''
  releaseImage()
  loadingImage.value = true
  try {
    const bytes = await attachmentApi.readImage(messageId.value.trim(), imageIndex.value)
    imageUrl.value = URL.createObjectURL(new Blob([bytes], { type: sniffImageMime(bytes) }))
  } catch (error) {
    imageError.value = readable(error)
  } finally {
    loadingImage.value = false
  }
}

function releaseImage(): void {
  if (!imageUrl.value) {
    return
  }
  URL.revokeObjectURL(imageUrl.value)
  imageUrl.value = ''
}

function readable(error: unknown): string {
  return error instanceof TransportError ? error.message : String(error)
}

/** 命令只回字节不带类型，按文件头魔数判定图片类型 */
function sniffImageMime(bytes: ArrayBuffer): string {
  const head = new Uint8Array(bytes.slice(0, 12))
  if (head[0] === 0x89 && head[1] === 0x50) {
    return 'image/png'
  }
  if (head[0] === 0xff && head[1] === 0xd8) {
    return 'image/jpeg'
  }
  if (head[0] === 0x47 && head[1] === 0x49) {
    return 'image/gif'
  }
  if (head[0] === 0x52 && head[1] === 0x49 && head[8] === 0x57) {
    return 'image/webp'
  }
  if (head[0] === 0x42 && head[1] === 0x4d) {
    return 'image/bmp'
  }
  return 'application/octet-stream'
}

onMounted(loadVersion)
onBeforeUnmount(releaseImage)
</script>

<template>
  <BaseWorkbench>
    <main class="spike">
      <h1 class="spike-title">工具链自检</h1>

    <section class="spike-card">
      <h2 class="spike-heading">运行时</h2>
      <p class="spike-line">
        <span class="spike-label">桌面桥接</span>
        <span class="spike-value">{{ runtimeReady ? '可用' : '不可用（浏览器直开属预期，须在桌面窗口内查看）' }}</span>
      </p>
      <p class="spike-line">
        <span class="spike-label">应用版本</span>
        <span class="spike-value">{{ version || '—' }}</span>
      </p>
      <p v-if="versionError" class="spike-error">{{ versionError }}</p>
      <button type="button" class="spike-button" @click="loadVersion">重新读取版本</button>
    </section>

    <section class="spike-card">
      <h2 class="spike-heading">外观</h2>
      <p class="spike-line">
        <span class="spike-label">当前主题</span>
        <span class="spike-value">{{ theme === 'dark' ? '深色' : '浅色' }}</span>
      </p>
      <BaseButton variant="secondary" @click="switchTheme">切换主题</BaseButton>
    </section>

    <section class="spike-card">
      <h2 class="spike-heading">二进制图片</h2>
      <label class="spike-field">
        <span class="spike-label">消息 ID</span>
        <input v-model="messageId" type="text" class="spike-input" placeholder="消息 UUID" />
      </label>
      <label class="spike-field">
        <span class="spike-label">图片序号</span>
        <input v-model.number="imageIndex" type="number" min="0" class="spike-input" />
      </label>
      <button type="button" class="spike-button" :disabled="loadingImage" @click="loadImage">
        {{ loadingImage ? '读取中…' : '读取图片' }}
      </button>
      <p v-if="imageError" class="spike-error">{{ imageError }}</p>
      <img v-if="imageUrl" :src="imageUrl" class="spike-image" alt="读取到的会话图片" />
    </section>
    </main>
  </BaseWorkbench>
</template>

<style scoped>
.spike {
  display: flex;
  flex-direction: column;
  gap: var(--gap-section);
}

.spike-title {
  font-family: var(--font-title);
  font-size: var(--fs-display);
  line-height: var(--lh-display);
  font-weight: var(--fw-semibold);
}

.spike-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  padding: var(--pad-card-lg);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.spike-heading {
  font-size: var(--fs-h3);
  line-height: var(--lh-h3);
  font-weight: var(--fw-semibold);
}

.spike-line {
  display: flex;
  gap: var(--space-12);
  font-size: var(--fs-body);
}

.spike-label {
  min-width: 72px;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.spike-value {
  color: var(--text-1);
}

.spike-field {
  display: flex;
  align-items: center;
  gap: var(--space-12);
}

.spike-input {
  flex: 1;
  height: 32px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-size: var(--fs-body);
}

.spike-input:focus-visible {
  outline: none;
  border-color: var(--brand-line);
  box-shadow: var(--focus-ring);
}

.spike-button {
  align-self: flex-start;
  height: 32px;
  padding: 0 var(--space-16);
  border: none;
  border-radius: var(--radius-btn);
  background: var(--btn-bg);
  color: var(--btn-fg);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
  box-shadow: var(--btn-shadow);
  cursor: pointer;
}

.spike-button:hover:not(:disabled) {
  background: var(--btn-bg-hover);
}

.spike-button:active:not(:disabled) {
  background: var(--btn-bg-active);
  transform: translateY(1px);
}

.spike-button:focus-visible {
  outline: none;
  box-shadow: var(--btn-shadow), var(--focus-ring);
}

.spike-button:disabled {
  opacity: var(--opacity-disabled);
  box-shadow: none;
  cursor: default;
}

.spike-error {
  color: var(--danger);
  font-size: var(--fs-small);
}

.spike-image {
  max-width: 100%;
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
}
</style>
