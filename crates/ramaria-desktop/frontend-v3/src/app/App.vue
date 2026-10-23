<script setup lang="ts">
/**
 * src/app/App.vue — 应用根组件
 *
 * 职责:
 * - 挂载视图出口与全局提示层
 * - 应用状态变化时跟随跳转全屏页 / 主界面
 *
 * 设计特点:
 * - 启动引导由路由守卫完成；本组件在引导结束前不跟随状态，避免用默认值误跳
 * - 状态跟随只在根组件一处 watch，视图不各自处理路由
 * - 模型下载不进全屏页（D-V30-044），只显示横幅，不触发跳转
 * - 圆键导航在全屏页同样常驻（D-V30-046）：品牌与主题切换由它承担
 */

import { watch } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'

import ToastLayer from '../components/base/ToastLayer.vue'
import NavDock from '../components/nav/NavDock.vue'
import { FULLSCREEN_ROUTES } from './router'
import { useAppStore } from '../stores/app'

const app = useAppStore()
const router = useRouter()
const route = useRoute()

watch(
  () => app.appState,
  (state) => {
    if (!app.booted) {
      return
    }
    const current = String(route.name ?? '')
    if (state === 'needs_setup' && current !== 'setup') {
      void router.replace({ name: 'setup' })
      return
    }
    if (state === 'fatal_error' && current !== 'error') {
      void router.replace({ name: 'error' })
      return
    }
    if ((state === 'ready' || state === 'degraded') && FULLSCREEN_ROUTES.has(current)) {
      void router.replace({ name: 'chat' })
    }
  },
  { immediate: true },
)
</script>

<template>
  <RouterView />
  <NavDock />
  <ToastLayer />
</template>
