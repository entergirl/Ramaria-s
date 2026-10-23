/**
 * vite.config.ts — 构建与开发服务器配置
 *
 * 设计特点:
 * - 资源一律输出为独立文件（`assetsInlineLimit: 0`），避免内联 data: 与严格 CSP 冲突
 * - 开发端口固定 1420，与 Tauri 覆盖配置的 devUrl 一致；端口占用直接失败而不静默换端口
 * - 构建目标为 WebView2 可用的 Chromium 版本，不使用更新的语法糖
 * - 源码不注入运行时求值，生产产物无需 `unsafe-eval`
 */

import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: 'chrome105',
    assetsInlineLimit: 0,
    sourcemap: false,
    chunkSizeWarningLimit: 900,
  },
})
