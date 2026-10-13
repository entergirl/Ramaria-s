/**
 * js/views/settings/shared.js — Ramaria 设置页共享构件
 *
 * 职责:
 * - 创建全局命名空间 window.RamariaSettingsNS（状态容器与各模块挂载点）
 * - 提供跨目录项共用的工具函数（DOM 查询 / 双写结果处理 / 复制 / 保存路径选择 / 版本加载）
 *
 * 设计特点:
 * - 各设置模块共用同一 NS.state；状态字段语义与原单文件设置页一致
 * - 本文件最先加载，其余模块按 index.html 的 script 顺序挂到 NS 上
 *
 * 依赖:
 * - RamariaStore / RamariaToast / RamariaApi
 * - TauriBridge
 */

(function (NS) {
    'use strict';

 // =========================================================
 // 共享状态
 // =========================================================

    /**
     * 跨文件共享状态（原设置页顶层状态变量；字段语义保持不变）。
     *
     * 说明:
     * - lastEntry / currentEntry 为目录导航新增（页签选中项记忆与当前渲染项）；
     * - embeddingConfig 缓存嵌入模型配置（切回目录项时回显用）。
     */
    NS.state = {
        /** Router 钩子注销函数集合（生命周期销毁用） */
        unregisterFns: [],
        /** 事件订阅取消函数集合（离开视图时统一清理） */
        unsubs: [],
        /** 当前后端配置缓存 */
        backendConfig: null,
        /** 当前嵌入模型配置缓存 */
        embeddingConfig: null,
        /** 当前隐私状态 */
        privacyStatus: null,
        /** MCP 接入信息缓存（get_mcp_info 结果：库路径 / CLI 命令 / 活动统计） */
        mcpInfo: null,
        /** MCP 客户端配置片段（随 mcpInfo 刷新重建） */
        mcpSnippets: [],
        /** 当前选中的客户端片段索引（客户端选择器） */
        mcpClientIndex: 0,
        /** 社交通道面板状态缓存（get_channel_status 结果） */
        channelStatus: null,
        /** 社交通道绑定列表缓存（list_channel_bindings 结果） */
        channelBindings: [],
        /** 通道绑定的可选人格列表缓存（改绑下拉用） */
        channelPersonas: [],
        /** 本次启动（会话）是否启用调试 */
        debugEnabled: false,
        /** 完整生效配置缓存（get_full_config 回显 + 保存时回写） */
        fullConfig: null,
        /** 各页签上次选中的目录项 key（会话内记忆） */
        lastEntry: { basic: 'backend', advanced: 'retrieval' },
        /** 当前渲染的目录项（{ tab, key, group }） */
        currentEntry: null,
    };

    /** 各设置模块的对外暴露面容器 */
    NS.parts = NS.parts || {};

 // =========================================================
 // DOM 快捷查询
 // =========================================================

    function $(id) { return document.getElementById(id); }

 /**
 * 读取当前启动的调试模式。
 *
 * 优先取 Store.debugEnabled（app.js 启动时写入）；缺省时回退解析
 * Store.settings（防御启动竞态）。
 */
    function _currentDebugEnabled() {
        var cached = RamariaStore.get('debugEnabled');
        if (typeof cached === 'boolean') return cached;
        var list = RamariaStore.get('settings');
        if (list && Array.isArray(list)) {
            for (var i = 0; i < list.length; i++) {
                if (list[i] && list[i].key === 'debug_enabled') {
                    var v = list[i].value;
                    return v === true || v === 'true' || v === '1';
                }
            }
        }
        return false;
    }

    /**
     * 复制文本到剪贴板。
     *
     * 策略:
     * 1. 优先 Clipboard API（Tauri WebView2 支持）；
     * 2. 失败/不可用时回退临时 textarea + execCommand（旧内核兜底）。
     *
     * 返回:
     * - `true`: 复制成功；`false`: 两条路径均失败（调用方提示手动复制）。
     */
    async function _copyText(text) {
        if (!text) return false;

        try {
            if (typeof navigator !== 'undefined' && navigator.clipboard && navigator.clipboard.writeText) {
                await navigator.clipboard.writeText(text);
                return true;
            }
        } catch (err) {
            console.warn('[SettingsView] Clipboard API 复制失败，尝试回退方案:', err);
        }

        try {
            var ta = document.createElement('textarea');
            ta.value = text;
            ta.setAttribute('readonly', 'readonly');
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            var ok = document.execCommand('copy');
            document.body.removeChild(ta);
            return ok;
        } catch (err) {
            console.error('[SettingsView] 复制失败:', err);
            return false;
        }
    }

    /** 免打扰时段格式（HH:MM-HH:MM，24 小时制，支持跨零点） */
    var _QUIET_HOURS_PATTERN = /^([01]\d|2[0-3]):[0-5]\d-([01]\d|2[0-3]):[0-5]\d$/;

    async function _pickSavePath(defaultName, filterName) {
 // 使用 Tauri dialog plugin 的原生保存对话框
        if (window.__TAURI__ && window.__TAURI__.dialog && window.__TAURI__.dialog.save) {
            try {
                var result = await window.__TAURI__.dialog.save({
                    defaultPath: defaultName,
                    filters: [{
                        name: filterName || '文件',
                        extensions: [defaultName.split('.').pop() || 'txt'],
                    }],
                });
 // 用户取消时 result 为 null
                return result || null;
            } catch (err) {
                console.warn('[SettingsView] 原生文件对话框失败:', err);
            }
        }

 // 尝试通过 Tauri invoke 调用
        if (TauriBridge && TauriBridge.invoke) {
            try {
                var invokeResult = await TauriBridge.invoke('save_file_dialog', {
                    default_name: defaultName,
                    filter_name: filterName,
                });
                if (invokeResult) return invokeResult;
            } catch (err) {
                console.warn('[SettingsView] 调用 save_file_dialog 失败:', err);
            }
        }

 // 最终降级：浏览器 prompt
        var path = prompt('请输入导出文件路径（例：C:\\Users\\YourName\\Desktop\\' + defaultName + ')', defaultName);
        return path;
    }

 // =========================================================
 // 版本加载（页面进入时静默调用）
 // =========================================================

 /**
 * 静默加载当前版本信息。
 *
 * 行为:
 * - 调用 getVersion API（纯本地，无网络请求，不消耗 GitHub API 配额）。
 * - 更新页面上的版本显示。
 * - 不显示 toast，不改变按钮状态。
 */
    async function _loadVersion() {
        try {
            var version = await RamariaApi.diagnostics.getVersion();

            var versionEl = $('settings-current-version');
            if (versionEl) versionEl.textContent = 'v' + (version || '?');

 // 同步更新"关于"区块的版本号
            var aboutVersionEl = $('settings-about-version');
            if (aboutVersionEl) aboutVersionEl.textContent = 'v' + (version || '?');
        } catch (_) {
 // 静默忽略加载失败
        }
    }

    /**
     * 高级字段的 DOM id。
     */
    function _advFieldId(group, f) {
        return 'settings-adv-' + group.key + '-' + f.path.join('-');
    }

    /**
     * 计算字段在完整配置对象中的绝对路径。
     *
     * 说明:
     * - 元数据里字段 `path` 为组内相对路径，组级 `section` 给出配置段前缀；
     *   读写完整 `RamariaConfig` JSON 必须使用本函数拼接结果。
     * - 历史缺陷：早期实现直接用 `f.path` 读写，导致 `[utt]`/`[retrieval]` 等
     *   组读到 undefined（表单恒显示默认值）、保存写到不存在的根键（静默无效）。
     *
     * 参数:
     * - `group`: 组元数据（含 section）。
     * - `f`: 字段元数据（含相对 path）。
     *
     * 返回:
     * - 绝对路径数组。
     */
    function _advConfigPath(group, f) {
        var section = Array.isArray(group.section) ? group.section : [];
        return section.concat(f.path);
    }

    /**
     * 同步数值输入框的"默认值浅色字符"状态（v1.5 M6 U 项增强）。
     *
     * 规则: 输入框当前值 === 字段默认值 → 添加 `is-default`（浅色字符，
     * 提示"当前为默认值"）；否则移除（深色主文字，表示已自定义）。
     * 数值统一按字符串比较（HTML input.value 恒为字符串，避免 1 与 1.0 误判差异）。
     */
    function _syncAdvancedInputDefault(input, f) {
        if (!input) return;
        var isDefault = String(input.value) === String(f.def);
        input.classList.toggle('is-default', isDefault);
    }

    /**
     * 统一处理 updateFull 双写结果（单侧写失败降级不阻塞，但需明确提示；
     * 决策记录见 docs/dev-1.5/v1.5-decisions.md）。
     *
     * 返回:
     * - `true`: 至少一侧生效（含单侧失败降级提示）。
     * - `false`: 双写均失败（致命，调用方应报错）。
     */
    function _handleUpdateFullResult(result, okMsg) {
        if (!result) return true;
        if (result.fileOk === false && result.dbOk === false) return false;
        if (result.fileOk === false) {
            RamariaToast.show('warning', okMsg + '（config.toml 同步失败，数据库侧已生效；下次启动校验会提示不一致）');
            return true;
        }
        if (result.dbOk === false) {
            RamariaToast.show('warning', okMsg + '（数据库侧同步失败，config.toml 已生效）');
            return true;
        }
        if (result.failures && result.failures.length > 0) {
            RamariaToast.show('warning', okMsg + '（部分同步项失败：' + result.failures.join('；') + '）');
            return true;
        }
        RamariaToast.show('success', okMsg);
        return true;
    }

 // =========================================================
 // 跨模块挂载面（供其它设置模块与视图入口调用）
 // =========================================================

    NS.$ = $;
    NS.currentDebugEnabled = _currentDebugEnabled;
    NS.advFieldId = _advFieldId;
    NS.advConfigPath = _advConfigPath;
    NS.syncAdvancedInputDefault = _syncAdvancedInputDefault;
    NS.handleUpdateFullResult = _handleUpdateFullResult;
    NS.copyText = _copyText;
    NS.pickSavePath = _pickSavePath;
    NS.loadVersion = _loadVersion;
    NS.QUIET_HOURS_PATTERN = _QUIET_HOURS_PATTERN;
})(window.RamariaSettingsNS = window.RamariaSettingsNS || {});
