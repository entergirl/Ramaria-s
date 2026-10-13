/**
 * js/views/settings/nav.js — Ramaria 设置页目录导航数据
 *
 * 职责:
 * - 基础设置 12 项 / 高级设置 18 项的目录分组与顺序定义
 * - 基础 ↔ 高级映射表（跨页签互相跳转）
 * - 深链解析与目录项解析纯函数
 *
 * 设计特点:
 * - 纯数据 + 纯函数，不触碰 DOM；供视图入口渲染与回归测试直接读取
 * - 目录项标题与顺序沿用原设置页小标题；高级项标题由分组元数据提供，避免两处维护
 *
 * 依赖:
 * - 无（shared.js 已创建 NS）
 */

(function (NS) {
    'use strict';

 // =========================================================
 // 目录表
 // =========================================================

    /**
     * 基础设置目录（5 组 12 项；标题沿用原小标题，顺序沿用原渲染顺序）。
     */
    var BASIC_GROUPS = [
        {
            title: '模型与检索',
            items: [
                { key: 'backend', title: '🔌 后端配置' },
                { key: 'embedding', title: '🧲 嵌入模型' },
                { key: 'memory_injection', title: '🧠 记忆注入' },
            ],
        },
        {
            title: '对话与主动',
            items: [
                { key: 'session', title: '💬 会话' },
                { key: 'proactive', title: '🗨️ 主动对话' },
            ],
        },
        {
            title: '社交与接入',
            items: [
                { key: 'channel', title: '💬 社交平台接入' },
                { key: 'mcp', title: '📡 MCP 接入' },
            ],
        },
        {
            title: '隐私与数据',
            items: [
                { key: 'privacy', title: '🔒 隐私设置' },
                { key: 'data_dir', title: '📂 数据目录' },
                { key: 'data', title: '💾 数据管理' },
            ],
        },
        {
            title: '系统',
            items: [
                { key: 'diagnostics', title: '🔧 诊断与更新' },
                { key: 'about', title: 'ℹ️ 关于' },
            ],
        },
    ];

    /**
     * 高级设置目录（6 组 18 项；key 对应高级分组元数据的组 key）。
     *
     * 说明: 目录项标题由高级分组元数据（groups 数组的 title）提供，此处只定义
     * 分组、组内顺序与组名。
     */
    var ADVANCED_GROUPS = [
        { title: '检索与索引', keys: ['retrieval', 'index'] },
        { title: '记忆管线', keys: ['decay', 'thresholds', 'event_extraction', 'l1-progressive'] },
        { title: '注入与装配', keys: ['utt', 'examples', 'bridge', 'injection_budget', 'layer_dedup'] },
        { title: '人格与知识', keys: ['inference', 'knowledge', 'style', 'inference-upgrade'] },
        { title: '主动与通道', keys: ['proactive', 'channel'] },
        { title: '系统', keys: ['logging'] },
    ];

    /**
     * 基础 → 高级映射（同域互相跳转；有映射的基础项底部渲染跳转行）。
     */
    var BASIC_TO_ADVANCED = {
        backend: ['retrieval', 'index'],
        embedding: ['retrieval'],
        session: ['decay', 'thresholds', 'l1-progressive'],
        proactive: ['proactive'],
        channel: ['channel'],
    };

 // =========================================================
 // 纯函数
 // =========================================================

    /**
     * 基础目录项 key 是否合法。
     */
    function isKnownBasicKey(key) {
        for (var i = 0; i < BASIC_GROUPS.length; i++) {
            for (var j = 0; j < BASIC_GROUPS[i].items.length; j++) {
                if (BASIC_GROUPS[i].items[j].key === key) return true;
            }
        }
        return false;
    }

    /**
     * 高级目录项 key 是否合法。
     */
    function isKnownAdvancedKey(key) {
        for (var i = 0; i < ADVANCED_GROUPS.length; i++) {
            for (var j = 0; j < ADVANCED_GROUPS[i].keys.length; j++) {
                if (ADVANCED_GROUPS[i].keys[j] === key) return true;
            }
        }
        return false;
    }

    /**
     * 解析目录项（tab + key → 条目信息）。
     *
     * 参数:
     * - `tab`: `basic` | `advanced`。
     * - `key`: 目录项 key。
     *
     * 返回:
     * - `{ tab, key, group, title }`: 命中；`group` 为一级分组名（面包屑用），
     *   `title` 为基础项标题（高级项为 null，由调用方从分组元数据取）；
     * - `null`: tab 或 key 未知（调用方据此降级到默认项）。
     */
    function resolveNavEntry(tab, key) {
        if (tab === 'basic' && isKnownBasicKey(key)) {
            for (var i = 0; i < BASIC_GROUPS.length; i++) {
                for (var j = 0; j < BASIC_GROUPS[i].items.length; j++) {
                    if (BASIC_GROUPS[i].items[j].key === key) {
                        return {
                            tab: 'basic',
                            key: key,
                            group: BASIC_GROUPS[i].title,
                            title: BASIC_GROUPS[i].items[j].title,
                        };
                    }
                }
            }
        }
        if (tab === 'advanced' && isKnownAdvancedKey(key)) {
            for (var k = 0; k < ADVANCED_GROUPS.length; k++) {
                if (ADVANCED_GROUPS[k].keys.indexOf(key) !== -1) {
                    return { tab: 'advanced', key: key, group: ADVANCED_GROUPS[k].title, title: null };
                }
            }
        }
        return null;
    }

    /**
     * 解析设置页深链（`#settings/{tab}/{key}` 两段式）。
     *
     * 参数:
     * - `hash`: location.hash 字符串（含 `#`）。
     *
     * 返回:
     * - `{ tab, key }`: 格式合法（tab ∈ basic|advanced；key 非空）；
     * - `null`: 格式非法（前缀 / 段数 / tab 不匹配）。
     *
     * 说明: key 是否存在于目录表由 resolveNavEntry 负责（未知 key 降级到默认项）。
     */
    function parseSettingsHash(hash) {
        if (typeof hash !== 'string') return null;
        var m = /^#settings\/(basic|advanced)\/([A-Za-z0-9_-]+)$/.exec(hash);
        if (!m) return null;
        return { tab: m[1], key: m[2] };
    }

    /**
     * 取某分组的首个目录项 key（面包屑分组链接跳转用）。
     *
     * 参数:
     * - `tab`: `basic` | `advanced`。
     * - `groupTitle`: 一级分组名。
     *
     * 返回:
     * - 分组首项 key；分组不存在返回 null。
     */
    function firstKeyOfGroup(tab, groupTitle) {
        if (tab === 'basic') {
            for (var i = 0; i < BASIC_GROUPS.length; i++) {
                if (BASIC_GROUPS[i].title === groupTitle && BASIC_GROUPS[i].items.length) {
                    return BASIC_GROUPS[i].items[0].key;
                }
            }
        } else if (tab === 'advanced') {
            for (var j = 0; j < ADVANCED_GROUPS.length; j++) {
                if (ADVANCED_GROUPS[j].title === groupTitle && ADVANCED_GROUPS[j].keys.length) {
                    return ADVANCED_GROUPS[j].keys[0];
                }
            }
        }
        return null;
    }

    /**
     * 基础 → 高级映射的反向表（由正向表推导，保证两向同源）。
     */
    var ADVANCED_TO_BASIC = {};
    (function buildReverse() {
        for (var basicKey in BASIC_TO_ADVANCED) {
            if (!Object.prototype.hasOwnProperty.call(BASIC_TO_ADVANCED, basicKey)) continue;
            var list = BASIC_TO_ADVANCED[basicKey];
            for (var i = 0; i < list.length; i++) {
                var advKey = list[i];
                if (!ADVANCED_TO_BASIC[advKey]) ADVANCED_TO_BASIC[advKey] = [];
                ADVANCED_TO_BASIC[advKey].push(basicKey);
            }
        }
    })();

 // =========================================================
 // 挂载面
 // =========================================================

    NS.NAV_BASIC = BASIC_GROUPS;
    NS.NAV_ADVANCED = ADVANCED_GROUPS;
    NS.CROSSLINKS = {
        basicToAdvanced: BASIC_TO_ADVANCED,
        advancedToBasic: ADVANCED_TO_BASIC,
    };
    NS.parseSettingsHash = parseSettingsHash;
    NS.resolveNavEntry = resolveNavEntry;
    NS.firstKeyOfGroup = firstKeyOfGroup;
})(window.RamariaSettingsNS = window.RamariaSettingsNS || {});
