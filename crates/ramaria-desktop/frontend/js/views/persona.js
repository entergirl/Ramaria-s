/**
 * js/views/persona.js — 人格管理视图 (Phase 6)
 *
 * 功能:
 * - 卡片网格展示所有人格（名称/类型/来源/描述/活跃时间）
 * - 点击卡片进入人格详情页（在线编辑基本信息）
 * - "设为默认对话人格"功能
 * - "重载性格"按钮触发记忆管线
 *
 * 设计特点:
 * - IIFE 模式 + 自动初始化 + Router 生命周期钩子
 * - 与 memory.js/settings.js 架构一致
 * - 异步数据加载，含骨架屏和错误兜底
 * - 编辑操作含 loading 态防重复提交
 *
 * 依赖:
 * - RamariaApi, RamariaStore, RamariaRouter, RamariaToast (可选)
 */

var RamariaPersonaView = (function () {
    'use strict';

    // =========================================================
    // 内部状态
    // =========================================================

    /** 当前展示页：'list' | 'detail' */
    var _page = 'list';
    /** 当前详情页的人格 UID */
    var _detailUid = null;
    /** 人格完整列表缓存 */
    var _personasFull = [];
    /** 是否正在加载 */
    var _loading = false;
    /** Router 钩子取消注册函数 */
    var _unregisterHooks = null;

    // =========================================================
    // 初始化与销毁
    // =========================================================

    /**
     * 初始化视图：注册 Router 生命周期钩子。
     * 由自动检测逻辑调用（等待 RamariaRouter 就绪）。
     */
    function init() {
        if (!window.RamariaRouter) {
            console.warn('[PersonaView] RamariaRouter 未就绪，延迟初始化');
            return;
        }

        _unregisterHooks = [
            RamariaRouter.registerHook('persona', 'enter', _onEnter),
            RamariaRouter.registerHook('persona', 'leave', _onLeave),
        ];

        console.log('[PersonaView] 初始化完成');
    }

    /**
     * 销毁视图：取消 Router 钩子和事件监听。
     */
    function destroy() {
        if (_unregisterHooks) {
            for (var i = 0; i < _unregisterHooks.length; i++) {
                _unregisterHooks[i]();
            }
            _unregisterHooks = null;
        }
        _page = 'list';
        _detailUid = null;
        _personasFull = [];
        console.log('[PersonaView] 已销毁');
    }

    // =========================================================
    // Router 钩子
    // =========================================================

    /**
     * enter 钩子：进入视图时加载数据并渲染。
     */
    function _onEnter() {
        RamariaRouter.setContentTitle('人格管理');
        RamariaRouter.setContentActions(''); // 清空头部操作区
        _loadAndRender();
    }

    /**
     * leave 钩子：离开视图时清理。
     */
    function _onLeave() {
        // 内存状态清理，DOM 由 Router 自动管理
        _page = 'list';
        _detailUid = null;
        // 不清空 _personasFull，下次进入时刷新
        _loading = false;
    }

    // =========================================================
    // 数据加载
    // =========================================================

    /**
     * 加载人格完整列表并渲染。
     * 实现加载态骨架屏、错误兜底和空状态。
     */
    async function _loadAndRender() {
        if (_loading) return;
        _loading = true;

        var container = document.getElementById('view-persona');
        if (!container) {
            _loading = false;
            return;
        }

        // 显示骨架屏
        container.innerHTML = _buildSkeleton();

        try {
            // 1. 加载人格完整数据
            _personasFull = await RamariaApi.persona.listFull();

            // 2. 加载 Store 中的 personas（用于记忆查询等现有功能）
            try {
                var storePersonas = await RamariaApi.memory.getPersonas();
                RamariaStore.set('personas', storePersonas, true); // 静默更新
            } catch (e) {
                console.warn('[PersonaView] 加载 Store personas 失败:', e.message);
            }

            // 3. 加载默认人格 UID（从 settings 中读取）
            try {
                var settings = await RamariaApi.config.getSettings();
                for (var i = 0; i < settings.length; i++) {
                    if (settings[i].key === 'default_persona_uid') {
                        RamariaStore.set('defaultPersonaUid', settings[i].value || null, true);
                        break;
                    }
                }
            } catch (e) {
                console.warn('[PersonaView] 加载默认人格设置失败:', e.message);
            }

            // 4. 渲染列表
            _page = 'list';
            _detailUid = null;
            container.innerHTML = '';

            if (_personasFull.length === 0) {
                container.appendChild(_buildEmptyState());
            } else {
                container.appendChild(_buildListPage());
            }
        } catch (err) {
            console.error('[PersonaView] 加载人格数据失败:', err);
            container.innerHTML = _buildErrorState(err.message || '未知错误');
        } finally {
            _loading = false;
        }
    }

    // =========================================================
    // 页面构建：列表页
    // =========================================================

    /**
     * 构建人格卡片网格列表页。
     *
     * 布局: 类 Bento Grid 自适应列，每张卡片包含:
     * - 人格名称 + 类型标签
     * - 来源渠道 + 简要描述预览
     * - 最后活跃时间
     * - 默认对话人格标记（⭐）
     * - hover 时高亮
     */
    function _buildListPage() {
        var wrapper = document.createElement('div');
        wrapper.className = 'persona-list-page';

        // ---- 工具栏 ----
        var toolbar = document.createElement('div');
        toolbar.className = 'persona-toolbar';
        toolbar.innerHTML =
            '<div class="persona-toolbar-title">' +
                '<span>共 ' + _personasFull.length + ' 个人格</span>' +
            '</div>';
        wrapper.appendChild(toolbar);

        // ---- 卡片网格 ----
        var grid = document.createElement('div');
        grid.className = 'persona-grid';

        var defaultUid = RamariaStore.get('defaultPersonaUid');

        for (var i = 0; i < _personasFull.length; i++) {
            var p = _personasFull[i];
            var card = _buildPersonaCard(p, defaultUid);
            grid.appendChild(card);
        }

        wrapper.appendChild(grid);
        return wrapper;
    }

    /**
     * 构建单张人格卡片。
     *
     * 参数:
     * - `p`: PersonaFullView 对象
     * - `defaultUid`: 当前默认对话人格 UID
     *
     * 返回: DOM 元素
     */
    function _buildPersonaCard(p, defaultUid) {
        var card = document.createElement('div');
        card.className = 'persona-card';
        card.setAttribute('role', 'button');
        card.setAttribute('tabindex', '0');
        card.setAttribute('aria-label', '查看人格 ' + p.name);

        // 是否为默认人格
        var isDefault = defaultUid && defaultUid === p.uid;

        // 类型标签样式
        var kindLabel = _kindLabel(p.kind);

        // 描述预览（截断至 80 字）
        var descPreview = '';
        if (p.description && p.description.trim()) {
            descPreview = p.description.length > 80
                ? p.description.substring(0, 80) + '...'
                : p.description;
        } else {
            descPreview = '<span class="persona-card-desc-empty">暂无描述</span>';
        }

        // 格式化时间
        var updatedStr = _formatTime(p.updated_at);

        // 构建 HTML
        card.innerHTML =
            '<div class="persona-card-header">' +
                '<div class="persona-card-icon" aria-hidden="true">' + _personaIcon(p.kind) + '</div>' +
                '<div class="persona-card-meta">' +
                    '<div class="persona-card-name">' +
                        _escapeHtml(p.name) +
                        (isDefault ? ' <span class="persona-card-badge-default" title="默认对话人格">⭐</span>' : '') +
                    '</div>' +
                    '<div class="persona-card-kind">' +
                        '<span class="persona-tag persona-tag--' + p.kind + '">' + kindLabel + '</span>' +
                        '<span class="persona-card-source">' + _escapeHtml(p.source) + '</span>' +
                    '</div>' +
                '</div>' +
            '</div>' +
            '<div class="persona-card-body">' +
                '<p class="persona-card-desc">' + descPreview + '</p>' +
            '</div>' +
            '<div class="persona-card-footer">' +
                '<span class="persona-card-time">' + updatedStr + '</span>' +
            '</div>';

        // 点击进入详情
        card.addEventListener('click', function () {
            _openDetail(p);
        });

        // 键盘无障碍：Enter/Space 触发
        card.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                _openDetail(p);
            }
        });

        return card;
    }

    // =========================================================
    // 页面构建：详情页
    // =========================================================

    /**
     * 打开人格详情页。
     *
     * 参数:
     * - `p`: PersonaFullView 对象
     */
    function _openDetail(p) {
        _page = 'detail';
        _detailUid = p.uid;
        RamariaRouter.setContentTitle(p.name);
        // 在内容头部注入返回按钮
        RamariaRouter.setContentActions(
            '<button class="btn btn-ghost btn-sm" id="persona-back-btn">← 返回列表</button>'
        );

        var container = document.getElementById('view-persona');
        if (!container) return;

        container.innerHTML = '';
        container.appendChild(_buildDetailPage(p));

        // 绑定返回按钮事件
        setTimeout(function () {
            var backBtn = document.getElementById('persona-back-btn');
            if (backBtn) {
                backBtn.addEventListener('click', _backToList);
            }
        }, 0);
    }

    /**
     * 返回列表页。
     */
    function _backToList() {
        RamariaRouter.setContentTitle('人格管理');
        RamariaRouter.setContentActions(''); // 清除头部返回按钮
        _page = 'list';
        _detailUid = null;

        var container = document.getElementById('view-persona');
        if (!container) return;

        if (_personasFull.length === 0) {
            container.innerHTML = '';
            container.appendChild(_buildEmptyState());
        } else {
            container.innerHTML = '';
            container.appendChild(_buildListPage());
        }
    }

    /**
     * 构建人格详情页。
     *
     * 包含区域:
     * - 返回按钮 + 标题
     * - 基本信息编辑表单（名称 / 头像 / 描述）
     * - 操作按钮区（保存 / 取消 / 设为默认 / 重载性格）
     * - 元数据区（uid / kind / source / ref_id / 时间）
     */
    function _buildDetailPage(p) {
        var defaultUid = RamariaStore.get('defaultPersonaUid');
        var isDefault = defaultUid && defaultUid === p.uid;

        var wrapper = document.createElement('div');
        wrapper.className = 'persona-detail-page';

        // ---- 表单 ----
        var form = document.createElement('form');
        form.className = 'persona-detail-form';
        form.addEventListener('submit', function (e) {
            e.preventDefault();
            _handleSave(p.uid);
        });

        // 名称字段
        form.appendChild(_buildField('名称', 'persona-name', 'text', p.name, '必填，人格显示名称', true));

        // 头像字段
        form.appendChild(_buildField('头像', 'persona-avatar', 'text', p.avatar || '', 'URL 或本地路径（可选）'));

        // 描述字段
        form.appendChild(_buildField('描述', 'persona-desc', 'textarea', p.description || '', '人格简要介绍（可选，最长 500 字）'));

        // ---- 操作按钮 ----
        var actions = document.createElement('div');
        actions.className = 'persona-detail-actions';

        // 保存按钮
        var saveBtn = document.createElement('button');
        saveBtn.type = 'submit';
        saveBtn.className = 'btn btn-primary';
        saveBtn.textContent = '保存修改';

        // 取消/返回按钮
        var cancelBtn = document.createElement('button');
        cancelBtn.type = 'button';
        cancelBtn.className = 'btn btn-secondary';
        cancelBtn.textContent = '返回列表';
        cancelBtn.addEventListener('click', _backToList);

        actions.appendChild(saveBtn);
        actions.appendChild(cancelBtn);

        // 设为默认 / 重载
        var extras = document.createElement('div');
        extras.className = 'persona-detail-extras';

        var setDefaultBtn = document.createElement('button');
        setDefaultBtn.type = 'button';
        setDefaultBtn.className = 'btn btn-secondary btn-sm';
        setDefaultBtn.textContent = isDefault ? '✓ 当前默认对话人格' : '设为默认对话人格';
        setDefaultBtn.disabled = isDefault;
        if (!isDefault) {
            setDefaultBtn.addEventListener('click', function () {
                _handleSetDefault(p.uid, setDefaultBtn);
            });
        }
        extras.appendChild(setDefaultBtn);

        var refreshBtn = document.createElement('button');
        refreshBtn.type = 'button';
        refreshBtn.className = 'btn btn-secondary btn-sm';
        refreshBtn.textContent = '重载性格画像';
        refreshBtn.addEventListener('click', function () {
            _handleRefresh(p, refreshBtn);
        });
        extras.appendChild(refreshBtn);

        // ---- 元数据 ----
        var meta = document.createElement('div');
        meta.className = 'persona-detail-meta';
        meta.innerHTML =
            '<div class="persona-meta-grid">' +
                '<div class="persona-meta-item"><span class="persona-meta-label">UID</span><span class="persona-meta-value">' + _escapeHtml(p.uid) + '</span></div>' +
                '<div class="persona-meta-item"><span class="persona-meta-label">类型</span><span class="persona-meta-value">' + _kindLabel(p.kind) + '</span></div>' +
                '<div class="persona-meta-item"><span class="persona-meta-label">来源</span><span class="persona-meta-value">' + _escapeHtml(p.source) + '</span></div>' +
                '<div class="persona-meta-item"><span class="persona-meta-label">来源ID</span><span class="persona-meta-value">' + (p.ref_id ? _escapeHtml(p.ref_id) : '—') + '</span></div>' +
                '<div class="persona-meta-item"><span class="persona-meta-label">创建时间</span><span class="persona-meta-value">' + _formatTime(p.created_at) + '</span></div>' +
                '<div class="persona-meta-item"><span class="persona-meta-label">更新时间</span><span class="persona-meta-value">' + _formatTime(p.updated_at) + '</span></div>' +
            '</div>';

        wrapper.appendChild(form);
        wrapper.appendChild(actions);
        wrapper.appendChild(extras);
        wrapper.appendChild(meta);

        return wrapper;
    }

    /**
     * 构建表单字段。
     *
     * 参数:
     * - `label`: 字段标签
     * - `id`: 元素 ID
     * - `type`: 'text' | 'textarea'
     * - `value`: 当前值
     * - `placeholder`: 占位提示
     * - `required`: 是否必填
     */
    function _buildField(label, id, type, value, placeholder, required) {
        var group = document.createElement('div');
        group.className = 'persona-field';

        var lbl = document.createElement('label');
        lbl.className = 'persona-field-label';
        lbl.htmlFor = id;
        lbl.textContent = label;
        if (required) {
            var req = document.createElement('span');
            req.className = 'persona-field-required';
            req.textContent = ' *';
            req.setAttribute('aria-hidden', 'true');
            lbl.appendChild(req);
        }
        group.appendChild(lbl);

        var input;
        if (type === 'textarea') {
            input = document.createElement('textarea');
            input.rows = 4;
            input.maxLength = 500;
        } else {
            input = document.createElement('input');
            input.type = 'text';
        }
        input.className = 'persona-field-input';
        input.id = id;
        input.name = id;
        input.value = value;
        input.placeholder = placeholder || '';
        if (required) input.required = true;

        group.appendChild(input);

        // 描述字数计数器
        if (type === 'textarea') {
            var counter = document.createElement('span');
            counter.className = 'persona-field-counter';
            counter.id = id + '-counter';
            counter.textContent = (value || '').length + ' / 500';
            input.addEventListener('input', function () {
                counter.textContent = input.value.length + ' / 500';
            });
            group.appendChild(counter);
        }

        return group;
    }

    // =========================================================
    // 操作处理
    // =========================================================

    /**
     * 保存人格信息。
     *
     * 参数:
     * - `uid`: 人格 UID
     *
     * 说明:
     * - 收集表单数据 → 调用 API → 刷新缓存 → 局部更新 DOM
     * - 保存按钮进入 loading 态防重复提交
     * - 失败时显示错误提示
     */
    async function _handleSave(uid) {
        var nameInput = document.getElementById('persona-name');
        var avatarInput = document.getElementById('persona-avatar');
        var descInput = document.getElementById('persona-desc');

        var name = nameInput ? nameInput.value.trim() : '';
        if (!name) {
            _showToast('名称不能为空', 'error');
            nameInput.focus();
            return;
        }

        // 构建更新请求
        var request = {};
        if (name) request.name = name;
        var avatarVal = avatarInput ? avatarInput.value.trim() : '';
        if (avatarVal) {
            request.avatar = avatarVal;
        }
        // description: 传空字符串表示清空
        if (descInput !== null) {
            request.description = descInput.value;
        }

        // loading 态
        var saveBtn = document.querySelector('.persona-detail-actions .btn-primary');
        if (saveBtn) {
            saveBtn.disabled = true;
            saveBtn.textContent = '保存中...';
        }

        try {
            var updated = await RamariaApi.persona.updateInfo(uid, request);

            // 刷新缓存
            for (var i = 0; i < _personasFull.length; i++) {
                if (_personasFull[i].uid === uid) {
                    _personasFull[i] = updated;
                    break;
                }
            }

            // 同步 Store 中的 personas
            try {
                var storePersonas = await RamariaApi.memory.getPersonas();
                RamariaStore.set('personas', storePersonas, true);
            } catch (e) { /* 非致命 */ }

            _showToast('修改已保存', 'success');

            // 刷新详情页展示
            var container = document.getElementById('view-persona');
            if (container && _page === 'detail' && _detailUid === uid) {
                container.innerHTML = '';
                container.appendChild(_buildDetailPage(updated));
            }
        } catch (err) {
            console.error('[PersonaView] 保存失败:', err);
            _showToast('保存失败: ' + (err.message || '未知错误'), 'error');
        } finally {
            if (saveBtn) {
                saveBtn.disabled = false;
                saveBtn.textContent = '保存修改';
            }
        }
    }

    /**
     * 设为默认对话人格。
     *
     * 参数:
     * - `uid`: 目标人格 UID
     * - `btn`: 按钮元素（用于更新 UI 状态）
     *
     * 说明:
     * - 将 `default_persona_uid` 写入 settings 表
     * - 更新 Store.defaultPersonaUid
     * - 列表页卡片上显示 ⭐ 标记
     */
    async function _handleSetDefault(uid, btn) {
        if (btn) {
            btn.disabled = true;
            btn.textContent = '设置中...';
        }

        try {
            await RamariaApi.config.updateSetting('default_persona_uid', uid);
            RamariaStore.set('defaultPersonaUid', uid);

            if (btn) {
                btn.disabled = true;
                btn.textContent = '✓ 当前默认对话人格';
            }

            _showToast('已设为默认对话人格', 'success');

            // 返回列表时卡片会刷新标记
        } catch (err) {
            console.error('[PersonaView] 设置默认人格失败:', err);
            _showToast('设置失败: ' + (err.message || '未知错误'), 'error');
            if (btn) {
                btn.disabled = false;
                btn.textContent = '设为默认对话人格';
            }
        }
    }

    /**
     * 重载性格画像（触发 L2→L3 管线）。
     *
     * 参数:
     * - `p`: PersonaFullView 对象
     * - `btn`: 按钮元素
     */
    async function _handleRefresh(p, btn) {
        if (btn) {
            btn.disabled = true;
            btn.textContent = '启动中...';
        }

        try {
            await RamariaApi.persona.refresh(p.uid);
            _showToast('记忆管线已启动，后台处理中。完成后性格画像将更新。', 'success');
        } catch (err) {
            console.error('[PersonaView] 刷新失败:', err);
            _showToast('启动失败: ' + (err.message || '未知错误'), 'error');
        } finally {
            if (btn) {
                btn.disabled = false;
                btn.textContent = '重载性格画像';
            }
        }
    }

    // =========================================================
    // UI 辅助：Toast
    // =========================================================

    /**
     * 显示 Toast 通知。
     * 兼容 RamariaToast（如果已加载），否则 fallback 到 console。
     */
    function _showToast(message, type) {
        if (window.RamariaToast && typeof RamariaToast.show === 'function') {
            RamariaToast.show(message, type || 'info');
        } else {
            console.log('[PersonaView Toast] ' + type + ': ' + message);
        }
    }

    // =========================================================
    // 状态页面构建
    // =========================================================

    /**
     * 构建加载态骨架屏（CSP-safe: 零内联 style）。
     */
    function _buildSkeleton() {
        var wrapper = document.createElement('div');
        wrapper.className = 'persona-list-page';

        var grid = document.createElement('div');
        grid.className = 'persona-grid';

        // 生成 4 个骨架卡片占位
        for (var i = 0; i < 4; i++) {
            var card = document.createElement('div');
            card.className = 'persona-card persona-card--skeleton';
            card.setAttribute('aria-hidden', 'true');

            // 使用 CSS 宽度工具类替代内联 style（w-60 / w-40 / w-90 / w-80 / w-30）
            var lines = [
                { cls: 'skeleton-line skeleton-line--lg w-60', mt: '' },
                { cls: 'skeleton-line w-40', mt: 'mt-2' },
                { cls: 'skeleton-line w-90', mt: 'mt-3' },
                { cls: 'skeleton-line w-80', mt: 'mt-1' },
                { cls: 'skeleton-line w-30', mt: 'mt-3' },
            ];
            for (var j = 0; j < lines.length; j++) {
                var div = document.createElement('div');
                div.className = lines[j].cls + (lines[j].mt ? ' ' + lines[j].mt : '');
                card.appendChild(div);
            }

            grid.appendChild(card);
        }

        wrapper.appendChild(grid);
        return wrapper.innerHTML; // 返回 HTML 字符串用于 innerHTML
    }

    /**
     * 构建空状态。
     */
    function _buildEmptyState() {
        var div = document.createElement('div');
        div.className = 'persona-empty';
        div.innerHTML =
            '<div class="persona-empty-icon" aria-hidden="true">👤</div>' +
            '<h3 class="persona-empty-title">暂无已注册人格</h3>' +
            '<p class="persona-empty-desc">人格将通过首次配置或数据导入自动创建</p>';
        return div;
    }

    /**
     * 构建错误状态。
     */
    function _buildErrorState(message) {
        var div = document.createElement('div');
        div.className = 'persona-error';
        div.innerHTML =
            '<div class="persona-error-icon" aria-hidden="true">⚠️</div>' +
            '<h3 class="persona-error-title">加载失败</h3>' +
            '<p class="persona-error-desc">' + _escapeHtml(message) + '</p>' +
            '<button class="btn btn-primary btn-sm persona-error-retry">重试</button>';
        div.querySelector('.persona-error-retry').addEventListener('click', function () {
            _loadAndRender();
        });
        return div;
    }

    // =========================================================
    // 格式化辅助
    // =========================================================

    /**
     * 格式化 Unix 毫秒时间为可读字符串。
     */
    function _formatTime(ms) {
        if (!ms) return '—';
        var d = new Date(ms);
        var y = d.getFullYear();
        var M = ('0' + (d.getMonth() + 1)).slice(-2);
        var day = ('0' + d.getDate()).slice(-2);
        var h = ('0' + d.getHours()).slice(-2);
        var min = ('0' + d.getMinutes()).slice(-2);
        return y + '-' + M + '-' + day + ' ' + h + ':' + min;
    }

    /**
     * 人格类型中文标签。
     */
    function _kindLabel(kind) {
        var map = {
            'user': '用户',
            'rama': 'Rama',
            'char': '角色',
            'anim': '动漫',
            'oc': 'OC',
            'hist': '历史',
        };
        return map[kind] || kind;
    }

    /**
     * 人格类型对应图标。
     */
    function _personaIcon(kind) {
        var map = {
            'user': '👤',
            'rama': '🍄',
            'char': '🎭',
            'anim': '✨',
            'oc': '✏️',
            'hist': '📜',
        };
        return map[kind] || '👤';
    }

    /**
     * 简单 HTML 转义（防 XSS）。
     */
    function _escapeHtml(str) {
        if (!str) return '';
        return str
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        init: init,
        destroy: destroy,
        /** 暴露 _loadAndRender 供外部强制刷新 */
        reload: _loadAndRender,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaPersonaView', {
    value: RamariaPersonaView,
    writable: false,
    configurable: false,
});

// =========================================================
// 自动初始化
// =========================================================

(function autoInit() {
    if (window.RamariaRouter) {
        RamariaPersonaView.init();
    } else {
        // 轮询等待 RamariaRouter 就绪
        var attempts = 0;
        var maxAttempts = 50;
        var interval = setInterval(function () {
            attempts++;
            if (window.RamariaRouter) {
                clearInterval(interval);
                RamariaPersonaView.init();
            } else if (attempts >= maxAttempts) {
                clearInterval(interval);
                console.error('[PersonaView] 等待 RamariaRouter 超时');
            }
        }, 200);
    }
})();
