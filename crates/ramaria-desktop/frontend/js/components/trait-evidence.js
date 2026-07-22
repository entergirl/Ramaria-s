/**
 * js/components/trait-evidence.js — 性格标签证据链可展开组件
 *
 * 职责:
 * - 为单条性格标签加载并渲染完整证据溯源链。
 * - 链结构: trait → 证据记录 → 事件 → L1 溯源 → evidence_notes。
 * - 支持逐层展开/折叠，默认全部折叠，点击逐层展开。
 *
 * 设计特点:
 * - 独立组件，通过 RamariaApi.memory.getEvidence() 获取数据。
 * - 处理加载中、空数据、错误三种状态。
 * - 置信度色条: 绿 ≥0.8 / 黄 0.6-0.8 / 橙 <0.6。
 * - 证据方向标记: ✓ 支持 / ✗ 矛盾 / - 中性。
 *
 * 依赖:
 * - RamariaApi.memory.getEvidence
 * - RamariaToast (错误提示)
 * - CSS: css/components/trait-evidence.css
 */

var RamariaTraitEvidence = (function () {
    'use strict';

// =========================================================
// 公开 API
// =========================================================

    /**
     * 获取并渲染证据链到指定容器。
     *
     * 参数:
     * - `container`: 要渲染到的 DOM 元素。
     * - `personaUid`: 所属人格 UID。
     * - `traitId`: 目标性格标签 ID。
     * - `traitLabel`: 标签名（用于显示）。
     */
    async function render(container, personaUid, traitId, traitLabel) {
        if (!container) {
            console.error('[TraitEvidence] container 为空');
            return;
        }

        // traitId 参数校验。
        // 当 trait.id=0（新推断 trait 尚未持久化获取自增 ID）或为 null/undefined 时，
        // 后端 getEvidence(traitId=0) 查询不到任何记录 → 返回空数组。
        // 需提前拦截无效参数，避免前端展示空白面板。
        if (!traitId || traitId === 0 || traitId === '0') {
            console.warn('[TraitEvidence] traitId 无效 (' + traitId + ')，该 trait 可能尚未持久化或为 mock 推断产物');
            container.innerHTML =
                '<div class="tev-empty">' +
                    '<div class="tev-empty-icon">🔄</div>' +
                    '<div class="tev-empty-text">证据数据暂未就绪</div>' +
                    '<div class="tev-empty-hint">该性格标签的证据记录尚未生成，请等待后台管线完成或重新导入数据。</div>' +
                '</div>';
            return;
        }

        // 显示加载中
        container.innerHTML =
            '<div class="tev-loading">' +
                '<span class="tev-loading-dot"></span>' +
                '<span class="tev-loading-dot"></span>' +
                '<span class="tev-loading-dot"></span>' +
                ' 加载证据链...' +
            '</div>';

        try {
            var result = await RamariaApi.memory.getEvidence(personaUid, traitId);
            if (!result || result.length === 0) {
                _renderEmpty(container, traitLabel);
                return;
            }

            var chain = result[0]; // get_trait_evidence 返回数组，取第一个元素

            if (!chain.evidence_events || chain.evidence_events.length === 0) {
                _renderEmpty(container, traitLabel);
                return;
            }

            _renderChain(container, chain, traitLabel);
        } catch (err) {
            console.error('[TraitEvidence] 加载证据链失败:', err);
            _renderError(container, err.message || '未知错误');
        }
    }

// =========================================================
// 渲染: 空状态
// =========================================================

    function _renderEmpty(container, traitLabel) {
        container.innerHTML =
            '<div class="tev-empty">' +
                '<div class="tev-empty-icon">📋</div>' +
                '<div class="tev-empty-text">' +
                    '「' + _escapeHtml(traitLabel) + '」暂无证据记录<br>' +
                    '<small>积累更多对话后，事件提取将自动关联证据</small>' +
                '</div>' +
            '</div>';
    }

// =========================================================
// 渲染: 错误状态
// =========================================================

    function _renderError(container, msg) {
        container.innerHTML =
            '<div class="tev-empty">' +
                '<div class="tev-empty-icon">⚠</div>' +
                '<div class="tev-empty-text">证据链加载失败<br><small>' + _escapeHtml(msg) + '</small></div>' +
            '</div>';
    }

// =========================================================
// 渲染: 完整证据链
// =========================================================

    function _renderChain(container, chain, traitLabel) {
        // 构建容器
        var html = '';

        // 头部统计
        html += '<div class="tev-header">';
        html += '<span class="tev-header-title">证据链: 「' + _escapeHtml(traitLabel) + '」</span>';
        html += '<span class="tev-header-stats">';
        if (chain.support_count > 0) {
            html += '<span class="tev-stat tev-stat--support">✓ ' + chain.support_count + ' 支持</span>';
        }
        if (chain.contradict_count > 0) {
            html += '<span class="tev-stat tev-stat--contradict">✗ ' + chain.contradict_count + ' 矛盾</span>';
        }
        if (chain.neutral_count > 0) {
            html += '<span class="tev-stat tev-stat--neutral">- ' + chain.neutral_count + ' 中性</span>';
        }
        html += '<span class="tev-stat">共 ' + chain.total_evidence + ' 条</span>';
        html += '</span></div>';

        // 逐条事件
        html += '<div class="tev-events">';
        for (var i = 0; i < chain.evidence_events.length; i++) {
            var ev = chain.evidence_events[i];
            html += _renderEventItem(ev, i);
        }
        html += '</div>';

        container.innerHTML = html;

        // 绑定展开/折叠事件
        _bindExpandEvents(container);
    }

// =========================================================
// 渲染: 单条事件
// =========================================================

    function _renderEventItem(ev, index) {
        var confClass = _confidenceClass(ev.confidence);
        var confPct = ev.confidence != null ? Math.round(ev.confidence * 100) : 0;
        var motivesHtml = ev.motives
            ? '<span class="tev-ev-motives">🎯 ' + _escapeHtml(ev.motives) + '</span>'
            : '';

        var html = '';

        // 事件头部（可点击展开）
        html += '<div class="tev-event" data-expand="event-' + index + '">';
        html += '<div class="tev-event-header">';
        html += '<span class="tev-event-expand-icon" id="tev-expand-icon-' + index + '">▶</span>';
        html += '<span class="tev-event-index">#' + (index + 1) + '</span>';
        html += '<span class="tev-event-title">' + _escapeHtml(ev.title || '(无标题)') + '</span>';
        html += '<span class="tev-ev-badges">';
        html += '<span class="tev-ev-badge tev-conf-badge ' + confClass + '">' + confPct + '%</span>';
        if (ev.attitude) {
            html += '<span class="tev-ev-badge tev-att-badge">' + _escapeHtml(ev.attitude) + '</span>';
        }
        html += '</span></div>';

        // 事件摘要
        html += '<div class="tev-event-summary">' + _escapeHtml(ev.summary || '') + '</div>';

        if (motivesHtml) {
            html += motivesHtml;
        }

        // 可展开详情节
        html += '<div class="tev-event-detail" id="tev-detail-' + index + '" style="display:none">';

        // 事件推断信号
        html += '<div class="tev-ev-signals">';
        html += '<span class="tev-signal">效价: ' + (ev.valence != null ? ev.valence.toFixed(2) : '-') + '</span>';
        html += '<span class="tev-signal">显著性: ' + (ev.salience != null ? ev.salience.toFixed(2) : '-') + '</span>';
        if (ev.paraphrase) {
            html += '<span class="tev-signal tev-signal--wide">重述: ' + _escapeHtml(ev.paraphrase) + '</span>';
        }
        html += '</div>';

        // L1 溯源列表
        if (ev.l1_sources && ev.l1_sources.length > 0) {
            html += '<div class="tev-l1-list">';
            html += '<div class="tev-l1-list-title">📝 源 L1 摘要 (' + ev.l1_sources.length + ')</div>';
            for (var j = 0; j < ev.l1_sources.length; j++) {
                var src = ev.l1_sources[j];
                html += _renderL1Source(src, index, j);
            }
            html += '</div>';
        } else {
            html += '<div class="tev-l1-list"><div class="tev-l1-list-title">📝 无 L1 溯源记录</div></div>';
        }

        html += '</div>'; // .tev-event-detail
        html += '</div>'; // .tev-event

        return html;
    }

// =========================================================
// 渲染: 单条 L1 溯源
// =========================================================

    function _renderL1Source(src, eventIndex, l1Index) {
        var detailId = 'tev-l1-detail-' + eventIndex + '-' + l1Index;
        var iconId = 'tev-l1-icon-' + eventIndex + '-' + l1Index;

        var html = '';
        html += '<div class="tev-l1-item">';
        html += '<div class="tev-l1-header" data-expand="' + detailId + '">';
        html += '<span class="tev-l1-expand-icon" id="' + iconId + '">▶</span>';
        html += '<span class="tev-l1-summary">' + _escapeHtml(_truncate(src.summary, 80)) + '</span>';
        html += '<span class="tev-l1-meta">权重: ' + (src.weight != null ? src.weight.toFixed(2) : '-') + '</span>';
        html += '</div>';

        // 可展开详情节: 完整摘要 + evidence_notes
        html += '<div class="tev-l1-detail" id="' + detailId + '" style="display:none">';
        html += '<div class="tev-l1-full-summary">' + _escapeHtml(src.summary) + '</div>';

        if (src.evidence_notes && src.evidence_notes.length > 0) {
            html += '<div class="tev-evidence-notes">';
            html += '<div class="tev-evidence-notes-title">📌 证据片段</div>';
            html += '<ul class="tev-evidence-notes-list">';
            for (var k = 0; k < src.evidence_notes.length; k++) {
                var note = src.evidence_notes[k];
                if (note && note.trim().length > 0) {
                    html += '<li>' + _escapeHtml(note) + '</li>';
                }
            }
            html += '</ul></div>';
        }

        html += '<div class="tev-l1-meta-row">';
        html += '<span>氛围: ' + (src.atmosphere || '-') + '</span>';
        html += '<span>效价: ' + (src.valence != null ? src.valence.toFixed(2) : '-') + '</span>';
        html += '</div>';

        html += '</div>'; // .tev-l1-detail
        html += '</div>'; // .tev-l1-item

        return html;
    }

// =========================================================
// 事件绑定: 展开/折叠
// =========================================================

    function _bindExpandEvents(container) {
        // 事件级展开
        var eventHeaders = container.querySelectorAll('.tev-event-header');
        for (var i = 0; i < eventHeaders.length; i++) {
            (function (header) {
                header.addEventListener('click', function () {
                    var eventEl = header.closest('.tev-event');
                    if (!eventEl) return;

                    var expandKey = eventEl.getAttribute('data-expand');
                    var detail = document.getElementById('tev-detail-' + expandKey.split('-')[1]);
                    var icon = document.getElementById('tev-expand-icon-' + expandKey.split('-')[1]);

                    if (detail) {
                        var isOpen = detail.style.display !== 'none';
                        detail.style.display = isOpen ? 'none' : 'block';
                        if (icon) icon.textContent = isOpen ? '▶' : '▼';
                        eventEl.classList.toggle('tev-event--expanded', !isOpen);
                    }
                });
            })(eventHeaders[i]);
        }

        // L1 级展开
        var l1Headers = container.querySelectorAll('.tev-l1-header');
        for (var j = 0; j < l1Headers.length; j++) {
            (function (header) {
                header.addEventListener('click', function (e) {
                    e.stopPropagation();
                    var detailId = header.getAttribute('data-expand');
                    var detail = document.getElementById(detailId);
                    if (!detail) return;

                    var iconId = detailId.replace('tev-l1-detail-', 'tev-l1-icon-');
                    var icon = document.getElementById(iconId);

                    var isOpen = detail.style.display !== 'none';
                    detail.style.display = isOpen ? 'none' : 'block';
                    if (icon) icon.textContent = isOpen ? '▶' : '▼';
                });
            })(l1Headers[j]);
        }
    }

// =========================================================
// 辅助函数
// =========================================================

    /** HTML 转义 */
    function _escapeHtml(str) {
        if (!str) return '';
        return String(str)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    /** 截断文本 */
    function _truncate(str, maxLen) {
        if (!str) return '';
        if (str.length <= maxLen) return str;
        return str.substring(0, maxLen) + '...';
    }

    /** 置信度 CSS 类 */
    function _confidenceClass(conf) {
        if (conf == null) return 'tev-conf--none';
        if (conf >= 0.8) return 'tev-conf--high';
        if (conf >= 0.6) return 'tev-conf--mid';
        return 'tev-conf--low';
    }

// =========================================================
// 公开 API
// =========================================================

    return {
        render: render,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaTraitEvidence', {
    value: RamariaTraitEvidence,
    writable: false,
    configurable: false,
});
