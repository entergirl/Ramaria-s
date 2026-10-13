/**
 * js/views/settings/sections/advanced/groups.js — Ramaria 设置页「高级设置」分组
 *
 * 职责:
 * - 18 个高级配置组元数据（字段默认值与 config/default.toml 逐键对齐）
 * - 固定区渲染（风险提示条 + 全局恢复默认栏）与单组渲染 / 回显 / 保存 / 重置
 * - order / whitelist 等复合字段的渲染与收集
 *
 * 设计特点:
 * - 组级保存与重置经 update_full_config 统一写入口双写；恢复全部默认无确认弹窗直接执行
 * - 数值输入框同步"默认值浅色字符"状态（is-default）；order 列表用事件委托支持行重建
 * - 目录项切换时仅渲染当前组（由视图入口驱动），固定区不随目录切换消失
 *
 * 依赖:
 * - NS.$ / NS.advFieldId / NS.advConfigPath / NS.syncAdvancedInputDefault /
 *   NS.handleUpdateFullResult / NS.QUIET_HOURS_PATTERN（shared.js）
 * - RamariaApi / RamariaToast / RamariaModal
 */

(function (NS) {
    'use strict';

    var state = NS.state;

 // =========================================================
 // 高级设置（v1.4 M6）
 // =========================================================

    /**
     * 高级配置组元数据（字段默认值与 ramaria-core/src/config.rs 的 Default 实现对齐）。
     *
     * 组约定:
     * - `key`: 组标识（DOM id 前缀；与配置段名不要求一致，如 `l1-progressive`）。
     * - `section`: 本组配置在完整 `RamariaConfig` JSON 中的段路径（数组）。
     *   字段的 `path` 为**组内相对路径**，读写时的完整路径 = `section.concat(path)`
     *   （见 [NS.advConfigPath]）。缺失该前缀会导致读写落到错误的键上：
     *   读取恒为空、保存静默无效（历史缺陷修复点）。
     * - `title` / `desc`: 分组标题与说明（静态文案，直接进 innerHTML）。
     *
     * 字段约定:
     * - `path`: 组内相对路径（数组，支持嵌套；完整路径由 `section` 前缀拼接）。
     * - `type`: `number` | `bool` | `whitelist` | `order` | `text`。
     * - `def`: 默认值（恢复默认与默认值标注用；`whitelist` / `order` 为数组）。
     * - `options`: `whitelist` / `order` 使用（可选值列表 {value, label}）。
     * - `order` 语义：数组顺序 = 保留优先级（高优先在前），UI 通过上/下移调整，
     *   与 `ramaria-core` 的 `[injection_budget].order`（InjectionSlot 数组）对应。
     */
    var _ADVANCED_GROUPS = [
        {
            key: 'retrieval',
            section: ['retrieval'],
            title: '🔍 检索参数',
            desc: '控制 L0/L1/L2 检索数量、相似度阈值与多通道融合权重，影响记忆召回质量。',
            fields: [
                { path: ['l0_window_size'], label: 'L0 滑动窗口', type: 'number', min: 1, def: 3, hint: 'L0 滑动窗口大小' },
                { path: ['l0_retrieve_top_k'], label: 'L0 检索条数', type: 'number', min: 0, def: 3, hint: 'L0 检索返回条数' },
                { path: ['l1_retrieve_top_k'], label: 'L1 检索条数', type: 'number', min: 0, def: 4, hint: 'L1 检索返回条数' },
                { path: ['l2_retrieve_top_k'], label: 'L2 检索条数', type: 'number', min: 0, def: 2, hint: 'L2 检索返回条数' },
                { path: ['similarity_threshold'], label: '相似度阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.6, hint: '余弦距离超过此值视为不相关' },
                { path: ['rrf_k'], label: 'RRF 平滑系数', type: 'number', min: 1, def: 60, hint: 'RRF 融合平滑系数 k' },
                { path: ['bm25_weight'], label: 'BM25 通道权重', type: 'number', step: 0.1, min: 0, def: 1.0, hint: 'BM25 通道权重' },
                { path: ['graph_weight'], label: '图谱通道权重', type: 'number', step: 0.1, min: 0, def: 0.8, hint: '图谱通道权重' },
                { path: ['retrieval_weight_l2'], label: 'L2 排序权重', type: 'number', step: 0.1, min: 0, def: 0.8, hint: '<1.0 表示 L2 优先展示' },
                { path: ['retrieval_weight_l1'], label: 'L1 排序权重', type: 'number', step: 0.1, min: 0, def: 1.0, hint: 'L1 结果排序权重' },
                // —— v2.0 新增：向量/关键词通道与摘要路 RAG 参数（阶段一不定稿，机制开关先接入）——
                { path: ['enable_vector'], label: '向量通道开关', type: 'bool', def: true, hint: 'false = 仅 BM25 + 图谱参与 RRF 融合' },
                { path: ['enable_keyword_channel'], label: '关键词镜像通道', type: 'bool', def: true, hint: 'false = 回退三通道（BM25 + 向量 + 图谱）' },
                { path: ['keyword_weight'], label: '关键词通道权重', type: 'number', step: 0.1, min: 0, def: 1.0, hint: '关键词镜像通道 RRF 权重（1.0 与向量同权）' },
                { path: ['narrative_weighted'], label: '脉络加权注入', type: 'bool', def: true, hint: 'v1.7 B4：按时间×话题相关性融合排序；false 回退取最近 N 条' },
                { path: ['narrative_top_k'], label: '脉络注入条数', type: 'number', min: 0, def: 3, hint: '脉络加权后注入的最大条数' },
                { path: ['rag_max_memories'], label: 'RAG 上下文条数', type: 'number', min: 0, def: 5, hint: '摘要路记忆上下文最大条目数' },
                { path: ['rag_max_summary_chars'], label: 'RAG 单条摘要字符', type: 'number', min: 0, def: 120, hint: '单条记忆摘要最大字符数' },
                { path: ['rag_share_threshold_user'], label: 'Persona-Aware user 阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.3, hint: 'user 类人格最低 share 过滤阈值' },
                { path: ['rag_share_threshold_char'], label: 'Persona-Aware 角色阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.5, hint: 'char/anim/oc/hist 类最低 share 过滤阈值' },
                { path: ['rag_share_threshold_rama'], label: 'Persona-Aware rama 阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.0, hint: 'rama 类最低 share 阈值（0 = 全量）' },
                { path: ['rag_include_graph_entities'], label: '上下文含图谱实体', type: 'bool', def: true, hint: '摘要路上下文格式化是否包含图谱实体' },
            ],
        },
        {
            key: 'decay',
            section: ['decay'],
            title: '⏳ 记忆衰减',
            desc: 'Ebbinghaus 遗忘曲线参数，控制记忆随时间衰减的速度。',
            fields: [
                { path: ['s_l0'], label: 'L0 稳定性系数', type: 'number', min: 1, def: 10, hint: '细节信息衰减最快' },
                { path: ['s_l1'], label: 'L1 稳定性系数', type: 'number', min: 1, def: 30, hint: 'L1 稳定性' },
                { path: ['s_l2'], label: 'L2 稳定性系数', type: 'number', min: 1, def: 60, hint: '聚合摘要衰减最慢' },
                { path: ['enable_access_boost'], label: '访问加成', type: 'bool', def: true, hint: '近期访问过的记忆保留率加成' },
                { path: ['recent_boost_days'], label: '近期访问加成天数', type: 'number', min: 0, def: 7, hint: '近期访问加成窗口' },
                { path: ['recent_boost_floor'], label: '近期保留率下限', type: 'number', step: 0.05, min: 0, max: 1, def: 0.5, hint: '近期访问保留率下限' },
                { path: ['salience_multiplier'], label: 'Salience 加成系数', type: 'number', step: 0.1, min: 0, def: 0.5, hint: 'S_adjusted = S × (1 + salience × multiplier)' },
            ],
        },
        {
            key: 'thresholds',
            section: ['thresholds'],
            title: '🎚️ 记忆层触发阈值',
            desc: '控制 L2 合并与 L3 推断的触发条件（计数 + 时间双路径）。',
            fields: [
                { path: ['l2_trigger_count'], label: 'L2 触发条数', type: 'number', min: 1, def: 5, hint: '未吸收 L1 达到此条数触发 L2 合并' },
                { path: ['l2_trigger_days'], label: 'L2 触发天数', type: 'number', min: 1, def: 7, hint: '最早未吸收 L1 超过此天数触发 L2' },
                { path: ['l3_trigger_count'], label: 'L3 触发条数', type: 'number', min: 1, def: 10, hint: '未吸收事件达到此条数触发 L3 推断' },
                { path: ['l3_trigger_days'], label: 'L3 触发天数', type: 'number', min: 1, def: 30, hint: '最早未吸收事件超过此天数触发 L3' },
                { path: ['cluster_delay_ms'], label: 'LLM 请求间隔（毫秒）', type: 'number', min: 0, def: 800, hint: '批量 LLM 请求间最小间隔（L1 导入/空闲封存摘要与 L2 事件提取共用），避免触发远程 API 速率限制（DeepSeek 建议调大）' },
            ],
        },
        {
            key: 'index',
            section: ['index'],
            title: '🗂️ 索引与 BM25',
            desc: 'BM25 增量合并与周期性重建节奏。',
            fields: [
                { path: ['bm25_incremental_threshold'], label: '增量合并阈值', type: 'number', min: 1, def: 10, hint: '缓冲区积累超过此条数触发合并' },
                { path: ['bm25_rebuild_interval'], label: '重建间隔（秒）', type: 'number', min: 10, def: 300, hint: 'BM25 定时重建检查间隔' },
            ],
        },
        {
            key: 'logging',
            section: ['logging'],
            title: '📜 日志',
            desc: '日志记录级别控制。',
            fields: [
                { path: ['log_full_prompt'], label: '记录完整 Prompt', type: 'bool', def: false, hint: '预留项：当前版本未接线——无论开关取值都不会记录完整 Prompt（后续启用前会先更新隐私说明）' },
            ],
        },
        {
            key: 'inference',
            section: ['inference'],
            title: '🔮 L3 推断',
            desc: 'Phase B/C 推断参数（温度、证据阈值、置信度、漂移检测、全量校准）。',
            fields: [
                { path: ['inferrer', 'temperature'], label: '推断温度', type: 'number', step: 0.1, min: 0, max: 2, def: 0.3, hint: 'Phase B LLM 生成温度' },
                { path: ['inferrer', 'max_tokens'], label: '最大输出 tokens', type: 'number', min: 128, def: 2048, hint: 'Phase B LLM 最大输出' },
                { path: ['inferrer', 'low_evidence_threshold'], label: '低证据阈值', type: 'number', step: 0.5, min: 0, def: 5.0, hint: '小样本分类证据阈值' },
                { path: ['inferrer', 'step_max_tokens'], label: '每步最大 tokens', type: 'number', min: 128, def: 2048, hint: 'Phase B 每步最大输出' },
                { path: ['confidence', 'stability_s'], label: '置信度稳定性 S', type: 'number', step: 1, min: 1, def: 60, hint: 'L2 层稳定性系数（Ebbinghaus）' },
                { path: ['confidence', 'min_decay'], label: '时间衰减保底', type: 'number', step: 0.01, min: 0, max: 1, def: 0.01, hint: '置信度时间衰减保底值' },
                { path: ['drift', 'alpha'], label: '漂移显著性水平', type: 'number', step: 0.01, min: 0.001, max: 1, def: 0.05, hint: 'Wasserstein 漂移检测显著性（锁定值）' },
                { path: ['drift', 'n_permutations'], label: '置换检验次数', type: 'number', min: 100, def: 1000, hint: '置换检验次数（锁定值）' },
                { path: ['calibration', 'round_threshold'], label: '校准轮次阈值', type: 'number', min: 1, def: 10, hint: '增量更新轮次阈值' },
                { path: ['calibration', 'event_doubling_ratio'], label: '事件翻倍比例', type: 'number', step: 0.1, min: 1, def: 2.0, hint: '事件量翻倍比例阈值' },
                { path: ['calibration', 'diff_alert_ratio'], label: '差异告警比例', type: 'number', step: 0.05, min: 0, max: 1, def: 0.3, hint: '差异告警比例' },
            ],
        },
        {
            key: 'event_extraction',
            section: ['event_extraction'],
            title: '📇 事件提取',
            desc: 'L1→L2 事件提取器的 LLM 参数（独立于对话参数，JSON 输出需更大 token 预算）。',
            fields: [
                { path: ['temperature'], label: '提取温度', type: 'number', step: 0.1, min: 0, max: 2, def: 0.3, hint: '事件提取 LLM 温度' },
                { path: ['max_tokens'], label: '最大输出 tokens', type: 'number', min: 256, def: 8192, hint: '事件 JSON 输出预算' },
                { path: ['max_events'], label: '单簇最大事件数', type: 'number', min: 1, def: 5, hint: '单簇最多提取的事件数' },
                { path: ['degraded_confidence_enabled'], label: '降级事件动态置信度', type: 'bool', def: true, hint: 'true = 降级事件按 min(0.59, 0.35 + 0.02 × n_l1) 计算置信度（恒 tentative）；false = 回退固定 0.5' },
            ],
        },
        {
            key: 'utt',
            section: ['utt'],
            title: '💬 utt 原文通道',
            desc: '原文话语块切分、检索与注入参数。原文是最高敏感层，白名单外 persona 不注入。',
            fields: [
                { path: ['enabled'], label: '启用原文通道', type: 'bool', def: true, hint: '关闭后行为回退 v1.3（不注入原文片段）' },
                { path: ['theta_gap_minutes'], label: '切分时间间隙（分钟）', type: 'number', min: 1, def: 10, hint: '相邻消息间隔超过此值切分为新块（窄切分，更细粒度）' },
                { path: ['max_msgs_per_block'], label: '单块最大消息数', type: 'number', min: 1, def: 80, hint: '超过此条数强制切分（更大块、更少切分）' },
                { path: ['retrieve_top_k'], label: '检索块数 top_k', type: 'number', min: 0, def: 3, hint: '对话时检索返回的 utt 块数量' },
                { path: ['max_block_chars'], label: '注入字符预算', type: 'number', min: 50, def: 1500, hint: '原文片段注入预算（超限按相似度丢整块）' },
                {
                    path: ['persona_kind_whitelist'],
                    label: '原文白名单（persona 类型）',
                    type: 'whitelist',
                    def: ['char', 'anim', 'oc', 'hist'],
                    options: [
                        { value: 'char', label: '角色' },
                        { value: 'anim', label: '动画' },
                        { value: 'oc', label: '原创 OC' },
                        { value: 'hist', label: '历史人物' },
                    ],
                    hint: '白名单外的 persona（助手/系统类）不注入原文',
                },
            ],
        },
        {
            key: 'examples',
            section: ['examples'],
            title: '🎭 示例注入',
            desc: 'Few-shot 示例的自学习抽取、评分轮换与兜底注入参数。',
            fields: [
                { path: ['enabled'], label: '启用示例注入', type: 'bool', def: true, hint: '关闭后回退 v1.3 静态 selected 查询' },
                { path: ['max_examples'], label: '最大示例条数', type: 'number', min: 1, def: 5, hint: '注入时的最大示例条数' },
            ],
        },
        {
            key: 'bridge',
            section: ['bridge'],
            title: '🌉 会话桥接',
            desc: '新会话加载上一会话尾部原文，保持对话连贯性。',
            fields: [
                { path: ['enabled'], label: '启用桥接', type: 'bool', def: true, hint: '关闭后新会话不加载桥接（等同 v1.3）' },
                { path: ['max_chars'], label: '桥接字符预算', type: 'number', min: 50, def: 800, hint: '超限从头部截断、保最近' },
            ],
        },
        {
            key: 'knowledge',
            section: ['knowledge'],
            title: '🧩 知识层（fact 路）',
            desc: '事件→知识事实抽取、判重与注入参数；本组为知识路独立检索参数（与摘要路/原文路互不串扰）。',
            fields: [
                { path: ['auto_fact_detect'], label: '自动事实检测总开关', type: 'bool', def: false, hint: 'v2.0：开启后抽取走常规轨道 + 增强轨道；false 与 v1.7 完全一致' },
                { path: ['detector_enabled'], label: '规则判定器开关', type: 'bool', def: true, hint: 'false = 不检索注入（零新增 LLM 调用）' },
                { path: ['retrieve_top_k'], label: '检索条数上限', type: 'number', min: 0, def: 0, hint: '0 = 与 v1.7 等价（不按条数截断，仅按预算注入）' },
                { path: ['retrieve_threshold'], label: '检索路由阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.0, hint: '0.0 = 与 v1.7 等价（沿用既有判定口径）' },
                { path: ['dedup_cosine_threshold'], label: '判重余弦阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.85, hint: '同 field 语义 ≥0.85 且关键词交集 ≥1 判重复' },
                { path: ['dedup_keyword_min'], label: '判重关键词下限', type: 'number', min: 1, def: 1, hint: '≥1 个共同词参与判重' },
                { path: ['corroboration_cosine_threshold'], label: '互证余弦阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.7, hint: '≥2 独立事件语义 ≥0.7 且 valence 一致才互证' },
                { path: ['injection_budget_chars'], label: '事实卡片注入预算（字符）', type: 'number', min: 0, def: 800, hint: '超预算保前部' },
                { path: ['volatile_halflife_days'], label: '动态事实时效半衰期（天）', type: 'number', min: 1, def: 30, hint: '随事件时间衰减' },
            ],
        },
        {
            key: 'style',
            section: ['style'],
            title: '🎨 说话风格（表达层）',
            desc: '五维风格统计、显著性检验与自动规则生成（A3）；关闭整链路回退 v1.6 语义。',
            fields: [
                { path: ['enabled'], label: '风格统计总开关', type: 'bool', def: true, hint: 'false = 不统计、不生成规则、不注入' },
                { path: ['auto_translate'], label: 'LLM 离线翻译增强', type: 'bool', def: true, hint: 'false = 仅模板拼接（确定性、零 LLM 依赖）' },
                { path: ['sample_fallback'], label: '小样本原文样例兜底', type: 'bool', def: true, hint: '样本不足时写入 SpeakingStyle 样例事实供展示' },
                { path: ['keyword_dict'], label: '关键词体系衔接', type: 'bool', def: true, hint: '词表存在时风格候选走词典增强；否则回退纯 bigram' },
                { path: ['min_sample_count'], label: '样本量阈值 n_p', type: 'number', min: 1, def: 200, hint: '低于此值标注数据不足，不生成规则、不注入' },
                { path: ['top_n'], label: '口癖/话题词 Top-N', type: 'number', min: 1, def: 10, hint: '文档范围 10~20' },
                { path: ['relative_boost_ratio'], label: '相对超频比阈值', type: 'number', step: 0.1, min: 0, def: 2.0, hint: 'persona 频率 / 全局频率 > 此值视为口癖' },
                { path: ['min_frequency'], label: '显著项最小频次', type: 'number', min: 1, def: 5, hint: '频次低于此值不参与显著性判定' },
                { path: ['z_critical'], label: 'z 临界值', type: 'number', step: 0.1, min: 0, def: 2.0, hint: '|z| ≥ 此值判定统计显著' },
            ],
        },
        {
            key: 'injection_budget',
            section: ['injection_budget'],
            title: '📦 注入协调预算（v2.0）',
            desc: 'RAG 摘要与四层注入纳入同一 token 池：超限按 order 从低优先整块丢弃。默认关闭时行为与既有版本等价。',
            fields: [
                { path: ['enabled'], label: '协调预算开关', type: 'bool', def: false, hint: 'false = 走既有独立预算路径（回归红线）' },
                { path: ['max_injection_tokens'], label: '注入总 token 上限', type: 'number', min: 0, def: 1000, hint: '固定骨架（能力边界/角色/时间）不计入池' },
                { path: ['max_rag_tokens'], label: 'RAG 独立 token 上限', type: 'number', min: 0, def: 0, hint: '0 = 不设独立上限，仅受总池约束' },
                {
                    path: ['order'],
                    label: '通道保留优先级（高优先在前）',
                    type: 'order',
                    def: ['rag', 'behavior', 'knowledge', 'style', 'memory'],
                    options: [
                        { value: 'rag', label: 'RAG 摘要' },
                        { value: 'behavior', label: '行为层' },
                        { value: 'knowledge', label: '知识层' },
                        { value: 'style', label: '表达层' },
                        { value: 'memory', label: '脉络层' },
                    ],
                    hint: '超预算时从列表末尾（最低优先）开始整块丢弃；未列出的通道视为最低优先',
                },
            ],
        },
        {
            key: 'layer_dedup',
            section: ['layer_dedup'],
            title: '🧾 层间去重仲裁（v2.0）',
            desc: '同一事实跨层去重与冲突仲裁（引用级 + 内容级覆盖）。默认关闭时走既有引用级去重路径。',
            fields: [
                { path: ['enabled'], label: '层间去重仲裁开关', type: 'bool', def: false, hint: 'false = 走既有引用级去重（行为不变）' },
            ],
        },
        {
            key: 'l1-progressive',
            section: ['l1', 'progressive'],
            title: '📚 渐进式摘要 B3（v2.0）',
            desc: '长会话（消息数/时间跨度超阈值）在封存时按段生成多个 L1。默认开启，关闭时回退 v1.6 行为。',
            fields: [
                { path: ['enabled'], label: '渐进式摘要开关', type: 'bool', def: true, hint: 'false = 整会话/按 utt 切分（v1.6）' },
                { path: ['msg_threshold'], label: '消息数触发阈值', type: 'number', min: 2, def: 100, hint: '超过此条数触发分段' },
                { path: ['span_hours'], label: '时间跨度阈值（小时）', type: 'number', min: 1, def: 24, hint: '首末消息跨度超过此值触发分段' },
                { path: ['tail_msg_count'], label: '尾段覆盖消息数', type: 'number', min: 1, def: 60, hint: '按此条数切段、全段生成（尾段覆盖最新）' },
            ],
        },
        {
            key: 'inference-upgrade',
            section: ['inference', 'upgrade'],
            title: '🔃 画像升级开关（v2.0）',
            desc: 'Phase A 后分层收缩与 Phase B/C 画像升级各增量的独立开关；全部关闭时画像输出回退旧版行为。',
            fields: [
                { path: ['cross_version_threshold_085'], label: '跨版本簇阈值 0.85', type: 'bool', def: true, hint: 'false = 回退旧值 0.75' },
                { path: ['cold_start_cross_user_prior'], label: '跨用户冷启动先验', type: 'bool', def: true, hint: 'false = 回退 persona 内先验' },
                { path: ['drift_restore_real_distribution'], label: '漂移检测真实恢复', type: 'bool', def: true, hint: 'false = 漂移检测整体显式跳过' },
                { path: ['causal_latency_emotion_trend'], label: '因果时延+情绪走势', type: 'bool', def: true, hint: 'false = 回退 v1.7 仅链长/循环模式' },
            ],
        },
        {
            key: 'proactive',
            section: ['proactive'],
            title: '🗨️ 主动对话（主动消息调度）',
            desc: '系统主动发起对话的调度节拍、打扰控制与选题权重；修改立即生效。',
            fields: [
                { path: ['enabled'], label: '主动对话总开关', type: 'bool', def: true, hint: '关闭后不调度、不投递' },
                { path: ['check_interval_seconds'], label: '调度检查间隔（秒）', type: 'number', min: 30, def: 300, hint: '决定「是否触发」的判定频率' },
                { path: ['min_idle_hours'], label: '最小空闲时长（小时）', type: 'number', def: 4, hint: '距上次对话满该时长才可能主动发起' },
                { path: ['daily_limit'], label: '每日投递上限（条）', type: 'number', def: 3, hint: '每个画像每日最多主动投递条数' },
                { path: ['daily_total_limit'], label: '全部人格每日合计上限（条）', type: 'number', min: 0, def: 0, hint: '全部人格合计每日主动投递上限；0 = 不限' },
                { path: ['quiet_hours'], label: '免打扰时段', type: 'text', def: '22:00-08:00', validate: 'quiet_hours', hint: 'HH:MM-HH:MM 格式，支持跨零点（如 22:00-08:00）' },
                { path: ['cooldown_hours'], label: '投递冷却（小时）', type: 'number', def: 8, hint: '两次主动投递之间的最短间隔' },
                { path: ['judge_enabled'], label: 'AI 判据开关', type: 'bool', def: true, hint: '关闭后由算法打分直接决策' },
                { path: ['judge_interval_hours'], label: '判据节流间隔（小时）', type: 'number', def: 3, hint: '两次判据调用之间的最短间隔' },
                { path: ['active_hours_weight'], label: '活跃时段加权强度', type: 'number', step: 0.1, min: 0, max: 1, def: 0.8, hint: '越高越偏向用户活跃时段' },
                { path: ['active_hours_window_days'], label: '活跃时段统计窗口（天）', type: 'number', def: 30, hint: '活跃时段建模的滚动窗口' },
                { path: ['active_hours_min_samples'], label: '时段建模样本门槛', type: 'number', def: 50, hint: '样本不足时不启用时段加权' },
                { path: ['rhythm_weight'], label: '人格节律加权强度', type: 'number', step: 0.1, min: 0, max: 1, def: 0.5, hint: '越高越让主动时机贴近人格自身作息；0 = 关闭人格节律' },
                { path: ['rhythm_window_days'], label: '人格活跃曲线统计窗口（天）', type: 'number', def: 90, hint: '人格活跃曲线建模的滚动窗口' },
                { path: ['rhythm_min_samples'], label: '人格活跃曲线样本门槛', type: 'number', def: 100, hint: '样本不足时不启用人格节律加权' },
                { path: ['valence_weight'], label: '效价入权强度', type: 'number', step: 0.1, min: 0, max: 1, def: 0.5, hint: '越高情绪波动大的记忆越优先' },
                { path: ['confidence_floor'], label: '事件置信度门槛', type: 'number', step: 0.1, min: 0, max: 1, def: 0.6, hint: '低于此值的事件不进入主动选题' },
                { path: ['light_touch_weight'], label: '轻触达兜底权重', type: 'number', step: 0.1, min: 0, max: 1, def: 0.3, hint: '轻触达兜底候选的选题权重' },
                { path: ['event_salience_threshold'], label: '事件显著性门槛', type: 'number', step: 0.1, min: 0, max: 1, def: 0.6, hint: '低于此值的事件不进入主动选题' },
                { path: ['event_window_days'], label: '事件时间窗（天）', type: 'number', def: 14, hint: '仅近 N 天内结束的事件参与选题' },
                { path: ['unresolved_valence_threshold'], label: '未了结负效价阈值', type: 'number', step: 0.1, min: -1, max: 0, def: -0.3, hint: 'valence 低于此值视为负面情绪事件' },
                { path: ['follow_up_days'], label: '跟进点（天）', type: 'number', def: 3, hint: '事件结束后第 N 天作为跟进时机' },
                { path: ['topic_cooldown_hours'], label: '选题去重冷却（小时）', type: 'number', def: 24, hint: '同一选题键在冷却窗口内不复选（与投递冷却独立）' },
                { path: ['silence_backoff_days'], label: '沉默退避阈值（天）', type: 'number', def: 3, hint: '连续未回应达到后进入退避（降频 / 暂停）' },
                { path: ['startup_grace_days'], label: '首次启用宽限期（天）', type: 'number', def: 3, hint: '首次启用后的宽限期内不触发' },
                { path: ['greeting_enabled'], label: '早安问候开关', type: 'bool', def: true, hint: '用户刚睡醒时段可主动问候' },
                { path: ['greeting_window_minutes'], label: '问候时机窗口（分钟）', type: 'number', def: 45, hint: '距用户每日首条消息中位时刻的允许偏差' },
                { path: ['recent_state_enabled'], label: '近期状态注入开关', type: 'bool', def: true, hint: '按最近记忆的心情影响回复语气' },
                { path: ['recent_state_window_days'], label: '近期状态统计窗口（天）', type: 'number', def: 7, hint: '近期状态统计的滚动窗口' },
            ],
        },
        {
            key: 'channel',
            section: ['channel'],
            title: '💬 社交通道（QQ / Telegram）',
            desc: '社交通道随桌面启动与外部平台收发消息；默认关闭。平台凭据（OneBot token / Telegram bot token）在「社交平台接入」面板中单独配置，不写入配置文件。',
            fields: [
                { path: ['enabled'], label: '通道总开关', type: 'bool', def: false, hint: '关闭时不连接任何平台' },
                { path: ['default_persona_uid'], label: '默认本机侧人格', type: 'text', def: 'rama-0001', hint: '绑定未覆盖时使用的对话人格 uid' },
                { path: ['qq_enabled'], label: 'QQ 平台开关', type: 'bool', def: false, hint: '需通道总开关开启' },
                { path: ['qq_ws_url'], label: 'QQ WS 地址', type: 'text', def: 'ws://127.0.0.1:3001', hint: 'OneBot v11 正向 WebSocket 地址（NapCat 服务端口）' },
                { path: ['telegram_enabled'], label: 'Telegram 平台开关', type: 'bool', def: false, hint: '需通道总开关开启' },
                { path: ['proactive_enabled'], label: '社交主动投递开关', type: 'bool', def: false, hint: '主动消息投递到社交通道的总开关；还须绑定级开关放行' },
            ],
        },
    ];

    /**
     * 渲染高级设置固定区（风险提示条 + 全局恢复默认栏）。
     *
     * 说明:
     * - 固定区位于内容区顶部、目录项内容之上，不随目录项切换消失；
     * - 单个配置组表单由 renderGroup 渲染（按目录项切换）。
     */
    function renderHeader(parent) {
        var risk = document.createElement('div');
        risk.className = 'settings-risk-banner';
        risk.textContent =
            '⚠️ 高级设置面向进阶用户与排障场景。修改以下参数可能影响检索与推断质量，' +
            '非排障场景请保持默认值；可单项或一键全部恢复默认。';
        parent.appendChild(risk);

        // ── 全局恢复默认栏（v1.5 M6 U 项功能增强）：单开一栏，点击直接执行（无弹窗）──
        var resetSection = document.createElement('div');
        resetSection.className = 'settings-section';
        resetSection.innerHTML =
            '<div class="settings-section-title">⚙️ 恢复默认</div>' +
            '<div class="settings-section-desc">将下方全部高级参数恢复为出厂默认值并立即保存（仅影响高级设置页参数，基础设置与后端连接配置不受影响）。</div>' +
            '<div class="settings-card">' +
                '<div class="settings-adv-reset-row">' +
                    '<span class="settings-adv-reset-desc">确认后立即生效，无需逐项操作。</span>' +
                    '<button class="btn btn-secondary btn-sm" id="settings-adv-reset-all">全部恢复默认</button>' +
                '</div>' +
            '</div>';
        parent.appendChild(resetSection);

        var resetAllBtn = NS.$('settings-adv-reset-all');
        if (resetAllBtn) {
            resetAllBtn.addEventListener('click', _handleAdvancedResetAll);
        }
    }

    /**
     * 按元数据渲染一个高级配置组。
     */
    function _renderAdvancedGroup(parent, group) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">' + group.title + '</div>' +
            '<div class="settings-section-desc">' + group.desc + '</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        var html = '';
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = NS.advFieldId(group, f);
            if (f.type === 'bool') {
                // 默认开的选项默认选中（v1.5 M6 U 项增强）：渲染时按 def 预置 checked，
                // 配置回显（_fillAdvancedForm）再覆盖为实际值，避免加载前/失败时状态与默认不符
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' +
                        '<input type="checkbox" id="' + fid + '"' + (f.def ? ' checked' : '') + ' /> ' + f.label +
                    '</label>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + (f.def ? '开' : '关') + '）</div>' +
                '</div>';
            } else if (f.type === 'whitelist') {
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<div class="settings-form-whitelist">';
                for (var w = 0; w < f.options.length; w++) {
                    var opt = f.options[w];
                    var defOn = f.def.indexOf(opt.value) !== -1;
                    html += '<label class="settings-form-inline-label">' +
                        '<input type="checkbox" id="' + fid + '-' + opt.value + '" data-value="' + opt.value + '"' +
                            (defOn ? ' checked' : '') + ' /> ' +
                        opt.label +
                    '</label>';
                }
                html += '</div>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def.join(', ') + '）</div>' +
                '</div>';
            } else if (f.type === 'order') {
                // 优先级列表：上下移动调整顺序（超预算时从末尾开始整块丢弃）
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<div class="settings-adv-order" id="' + fid + '" role="list">' +
                        _orderItemsHtml(f, f.def) +
                    '</div>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def.join(' → ') + '）</div>' +
                '</div>';
            } else if (f.type === 'text') {
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<input class="settings-form-input" id="' + fid + '" type="text"' +
                        (f.placeholder ? ' placeholder="' + f.placeholder + '"' : '') +
                        ' value="' + f.def + '" />' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def + '）</div>' +
                '</div>';
            } else {
                // 数值输入框：初始 value 预置默认值并以浅色字符显示（is-default），
                // 提示"当前为默认值"；用户输入不同值后自动转深色（见 input 事件同步）
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<input class="settings-form-input is-default" id="' + fid + '" type="number" value="' + f.def + '"' +
                        (f.step ? ' step="' + f.step + '"' : '') +
                        (f.min !== undefined ? ' min="' + f.min + '"' : '') +
                        (f.max !== undefined ? ' max="' + f.max + '"' : '') +
                    ' />' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def + '）</div>' +
                '</div>';
            }
        }
        html += '<div class="settings-save-hint">' +
            '<button class="btn btn-primary btn-sm" id="settings-adv-save-' + group.key + '">保存</button> ' +
            '<button class="btn btn-secondary btn-sm" id="settings-adv-reset-' + group.key + '">恢复默认</button>' +
        '</div>';
        card.innerHTML = html;
        section.appendChild(card);
        parent.appendChild(section);

        var saveBtn = NS.$('settings-adv-save-' + group.key);
        if (saveBtn) {
            saveBtn.addEventListener('click', function () { _handleAdvancedSave(group); });
        }
        var resetBtn = NS.$('settings-adv-reset-' + group.key);
        if (resetBtn) {
            resetBtn.addEventListener('click', function () { _handleAdvancedReset(group); });
        }

        // 数值输入框：输入时同步"默认值浅色字符"状态（值=默认 → 浅色，否则深色）
        for (var j = 0; j < group.fields.length; j++) {
            var field = group.fields[j];
            if (field.type === 'number') {
                var numInput = NS.$(NS.advFieldId(group, field));
                if (numInput) {
                    numInput.addEventListener('input', (function (input, fld) {
                        return function () { NS.syncAdvancedInputDefault(input, fld); };
                    })(numInput, field));
                }
            }
        }

        // 优先级列表：事件委托（容器内上下移动，重建行后无需重新绑定）
        for (var k = 0; k < group.fields.length; k++) {
            if (group.fields[k].type === 'order') {
                var orderBox = NS.$(NS.advFieldId(group, group.fields[k]));
                if (orderBox) _bindOrderField(orderBox);
            }
        }

        // log_full_prompt 开启需显式隐私确认
        var logBox = NS.$('settings-adv-logging-log_full_prompt');
        if (logBox) {
            logBox.addEventListener('change', function () {
                if (!logBox.checked) return;
                RamariaModal.show({
                    title: '⚠️ 隐私确认',
                    body: '「记录完整 Prompt」为预留项：当前版本未接线，开启或关闭都不会把完整 prompt ' +
                        '写入日志。确认仅保存开关取值，供后续版本启用时生效（届时会先更新隐私说明）。',
                    actions: [
                        { label: '取消', action: 'cancel', className: 'btn btn-secondary' },
                        { label: '确认开启', action: 'confirm', className: 'btn btn-danger' },
                    ],
                    onAction: function (action) {
                        if (action !== 'confirm') {
                            logBox.checked = false;
                        }
                    },
                });
            });
        }
    }

    /**
     * 按 value 查 `order` 字段的选项元数据（{value, label}）。
     *
     * 返回:
     * - 命中项；未命中返回 null（调用方据此过滤未知通道）。
     */
    function _orderOption(f, value) {
        for (var i = 0; i < f.options.length; i++) {
            if (f.options[i].value === value) return f.options[i];
        }
        return null;
    }

    /**
     * 归一化 `order` 字段的取值列表。
     *
     * 规则（与 ramaria-core `[injection_budget].order` 语义对齐）:
     * - 非数组/空数组 → 回退默认顺序；
     * - 过滤未知通道；重复项取首次出现位置；
     * - 配置缺失的通道按选项表顺序补到末尾，保证 UI 五项齐全、可上下移动。
     *
     * 参数:
     * - `f`: 字段元数据（含 options/def）。
     * - `value`: 配置中的当前值（可能 undefined）。
     *
     * 返回:
     * - 归一化后的通道顺序数组。
     */
    function _normalizeOrder(f, value) {
        var raw = Array.isArray(value) ? value : f.def;
        var out = [];
        for (var i = 0; i < raw.length; i++) {
            var v = raw[i];
            if (typeof v !== 'string') continue;
            if (!_orderOption(f, v)) continue;
            if (out.indexOf(v) !== -1) continue;
            out.push(v);
        }
        for (var j = 0; j < f.options.length; j++) {
            if (out.indexOf(f.options[j].value) === -1) out.push(f.options[j].value);
        }
        return out;
    }

    /**
     * 生成优先级列表的行 HTML（标签均为静态元数据，非用户输入）。
     *
     * 结构:
     * - `.settings-adv-order-item[data-value]`：一行（序号 + 名称 + 上/下移按钮）；
     * - 首行禁用「上移」、末行禁用「下移」，防止无效操作。
     */
    function _orderItemsHtml(f, values) {
        var order = _normalizeOrder(f, values);
        var html = '';
        for (var i = 0; i < order.length; i++) {
            var opt = _orderOption(f, order[i]);
            if (!opt) continue;
            var upDisabled = i === 0 ? ' disabled' : '';
            var downDisabled = i === order.length - 1 ? ' disabled' : '';
            html += '<div class="settings-adv-order-item" data-value="' + opt.value + '" role="listitem">' +
                '<span class="settings-adv-order-index">' + (i + 1) + '</span>' +
                '<span class="settings-adv-order-name">' + opt.label + '</span>' +
                '<span class="settings-adv-order-actions">' +
                    '<button type="button" class="btn btn-secondary btn-sm settings-adv-order-btn" data-dir="up" title="上移（提高保留优先级）"' + upDisabled + '>↑</button>' +
                    '<button type="button" class="btn btn-secondary btn-sm settings-adv-order-btn" data-dir="down" title="下移（降低保留优先级）"' + downDisabled + '>↓</button>' +
                '</span>' +
            '</div>';
        }
        return html;
    }

    /**
     * 绑定优先级列表的上/下移交互（事件委托，容器内行重建后无需重新绑定）。
     */
    function _bindOrderField(container) {
        container.addEventListener('click', function (ev) {
            // 沿父链定位带 data-dir 的按钮（不依赖 closest，兼容性更稳）
            var target = ev.target;
            var btn = null;
            while (target && target !== container) {
                if (target.getAttribute && target.getAttribute('data-dir')) {
                    btn = target;
                    break;
                }
                target = target.parentNode;
            }
            if (!btn) return;
            ev.preventDefault();

            var row = btn.parentNode ? btn.parentNode.parentNode : null;  // .settings-adv-order-item
            if (!row || row.parentNode !== container) return;

            var dir = btn.getAttribute('data-dir');
            if (dir === 'up' && row.previousElementSibling) {
                container.insertBefore(row, row.previousElementSibling);
            } else if (dir === 'down' && row.nextElementSibling) {
                container.insertBefore(row.nextElementSibling, row);
            }
            _refreshOrderIndexes(container);
        });
    }

    /**
     * 刷新优先级列表的序号与首尾按钮禁用态（移动后调用）。
     */
    function _refreshOrderIndexes(container) {
        var rows = container.querySelectorAll('.settings-adv-order-item');
        if (rows.length === 0) return;
        for (var i = 0; i < rows.length; i++) {
            var idxEl = rows[i].querySelector('.settings-adv-order-index');
            if (idxEl) idxEl.textContent = String(i + 1);

            var up = rows[i].querySelector('button[data-dir="up"]');
            var down = rows[i].querySelector('button[data-dir="down"]');
            if (up) up.disabled = (i === 0);
            if (down) down.disabled = (i === rows.length - 1);
        }
    }

    /**
     * 从配置对象读取嵌套路径值。
     */
    function _advGetValue(cfg, path) {
        var v = cfg;
        for (var i = 0; i < path.length; i++) {
            v = v[path[i]];
            if (v === undefined) return undefined;
        }
        return v;
    }

    /**
     * 向配置对象写入嵌套路径值。
     *
     * v1.5 M6 增强：路径中间对象缺失时自动补建（防御——配置快照可能不含
     * 某些嵌套分组，直接写入会抛 TypeError 中断「恢复全部默认」流程）。
     */
    function _advSetValue(cfg, path, value) {
        var v = cfg;
        for (var i = 0; i < path.length - 1; i++) {
            if (v[path[i]] == null || typeof v[path[i]] !== 'object') {
                v[path[i]] = {};
            }
            v = v[path[i]];
        }
        v[path[path.length - 1]] = value;
    }

    /**
     * 回显一个高级配置组（进入设置页时调用）。
     *
     * 说明（v1.5 M6 U 项增强）:
     * - 渲染时已按默认值预置控件状态；此处仅在实际配置存在对应字段时覆盖，
     *   配置缺失（undefined）时保留默认态（checkbox 保持默认选中、输入框保持默认值）。
     * - 数值输入框回显后同步"默认值浅色字符"状态。
     */
    function _fillAdvancedForm(group, cfg) {
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = NS.advFieldId(group, f);
            var value = _advGetValue(cfg, NS.advConfigPath(group, f));
            if (f.type === 'bool') {
                if (value !== undefined) {
                    var box = NS.$(fid);
                    if (box) box.checked = !!value;
                }
            } else if (f.type === 'whitelist') {
                if (value !== undefined) {
                    var list = Array.isArray(value) ? value : [];
                    for (var w = 0; w < f.options.length; w++) {
                        var cb = NS.$(fid + '-' + f.options[w].value);
                        if (cb) cb.checked = list.indexOf(f.options[w].value) !== -1;
                    }
                }
            } else if (f.type === 'order') {
                // 重建整列：归一化（过滤未知通道 + 补齐缺失项）后按配置顺序渲染
                var orderBox = NS.$(fid);
                if (orderBox) {
                    orderBox.innerHTML = _orderItemsHtml(f, value);
                }
            } else if (f.type === 'text') {
                var textInput = NS.$(fid);
                if (textInput && value !== undefined) textInput.value = value;
            } else {
                var input = NS.$(fid);
                if (!input) continue;
                if (value !== undefined) input.value = value;
                NS.syncAdvancedInputDefault(input, f);
            }
        }
    }

    /**
     * 回显全部高级配置组。
     */
    function _fillAdvancedForms(cfg) {
        for (var i = 0; i < _ADVANCED_GROUPS.length; i++) {
            _fillAdvancedForm(_ADVANCED_GROUPS[i], cfg);
        }
    }

    /**
     * 收集一个高级配置组的当前表单值（校验后返回 {path, value} 列表；
     * 校验失败返回 null 并 toast 提示）。
     */
    function _collectAdvancedGroup(group) {
        var entries = [];
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = NS.advFieldId(group, f);
            var value;
            if (f.type === 'bool') {
                var box = NS.$(fid);
                if (!box) continue;
                value = box.checked;
            } else if (f.type === 'whitelist') {
                var list = [];
                for (var w = 0; w < f.options.length; w++) {
                    var cb = NS.$(fid + '-' + f.options[w].value);
                    if (cb && cb.checked) list.push(f.options[w].value);
                }
                value = list;
            } else if (f.type === 'order') {
                var orderContainer = NS.$(fid);
                if (!orderContainer) continue;
                var orderRows = orderContainer.querySelectorAll('.settings-adv-order-item');
                var orderList = [];
                for (var k = 0; k < orderRows.length; k++) {
                    var slot = orderRows[k].getAttribute('data-value');
                    // 去重保持首次出现位置（与后端「重复项取首个位置」口径一致）
                    if (slot && orderList.indexOf(slot) === -1) orderList.push(slot);
                }
                if (orderList.length === 0) {
                    RamariaToast.show('warning', f.label + ' 不能为空');
                    return null;
                }
                value = orderList;
            } else if (f.type === 'text') {
                var textInput = NS.$(fid);
                if (!textInput) continue;
                var text = textInput.value.trim();
                if (text === '') {
                    RamariaToast.show('warning', f.label + ' 不能为空');
                    return null;
                }
                // 可选格式校验：由字段元数据的 validate 标记声明（如免打扰时段）
                if (f.validate === 'quiet_hours' && !NS.QUIET_HOURS_PATTERN.test(text)) {
                    RamariaToast.show('warning', f.label + ' 格式应为 HH:MM-HH:MM（如 22:00-08:00）');
                    return null;
                }
                value = text;
            } else {
                var input = NS.$(fid);
                if (!input) continue;
                var raw = input.value;
                if (raw === '') {
                    RamariaToast.show('warning', f.label + ' 不能为空');
                    return null;
                }
                value = (f.step && f.step < 1) ? parseFloat(raw) : parseInt(raw, 10);
                if (isNaN(value)) {
                    RamariaToast.show('warning', f.label + ' 不是有效数字');
                    return null;
                }
                // 范围校验（HTML min/max 在无 form 提交时不生效，需显式检查）
                if (f.min !== undefined && value < f.min) {
                    RamariaToast.show('warning', f.label + ' 不能小于 ' + f.min);
                    return null;
                }
                if (f.max !== undefined && value > f.max) {
                    RamariaToast.show('warning', f.label + ' 不能大于 ' + f.max);
                    return null;
                }
            }
            entries.push({ path: NS.advConfigPath(group, f), value: value });
        }
        return entries;
    }

    /**
     * 保存一个高级配置组（统一写入口双写）。
     */
    async function _handleAdvancedSave(group) {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var entries = _collectAdvancedGroup(group);
            if (!entries) return;

            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            for (var i = 0; i < entries.length; i++) {
                _advSetValue(cfg, entries[i].path, entries[i].value);
            }

            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, group.title + ' 已保存')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

    /**
     * 恢复一个高级配置组的默认值（立即保存，统一写入口双写）。
     */
    async function _handleAdvancedReset(group) {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            for (var i = 0; i < group.fields.length; i++) {
                var f = group.fields[i];
                _advSetValue(cfg, NS.advConfigPath(group, f), JSON.parse(JSON.stringify(f.def)));
            }

            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, group.title + ' 已恢复默认并保存')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;
            _fillAdvancedForm(group, cfg);
        } catch (err) {
            RamariaToast.show('error', '恢复默认失败', err.message || '未知错误');
        }
    }

    /**
     * 一键恢复全部高级设置参数为默认值（v1.5 M6 U 项功能增强）。
     *
     * 交互（2026-08-14 负责人调整：单开一栏、无确认弹窗）:
     * - 独立栏内直接点击执行（栏内已带说明文字，无需再弹窗确认）；
     * - 遍历 `_ADVANCED_GROUPS` 全部字段写回默认值并立即保存（统一写入口双写）；
     * - 保存成功后重新回显全部高级表单（触发"默认值浅色字符"状态同步）。
     */
    async function _handleAdvancedResetAll() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            for (var i = 0; i < _ADVANCED_GROUPS.length; i++) {
                var group = _ADVANCED_GROUPS[i];
                for (var j = 0; j < group.fields.length; j++) {
                    var f = group.fields[j];
                    _advSetValue(cfg, NS.advConfigPath(group, f), JSON.parse(JSON.stringify(f.def)));
                }
            }
            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, '高级设置已全部恢复默认并保存')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;
            _fillAdvancedForms(cfg);
        } catch (err) {
            RamariaToast.show('error', '恢复默认失败', err.message || '未知错误');
        }
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.advanced = {
        /** 渲染高级设置固定区（风险提示条 + 全局恢复默认栏） */
        renderHeader: renderHeader,
        /** 渲染一个高级配置组 */
        renderGroup: _renderAdvancedGroup,
        /** 回显一个高级配置组 */
        fillGroup: _fillAdvancedForm,
        /** 回显全部高级配置组（遍历全部组，未渲染组自动跳过） */
        fillAll: _fillAdvancedForms,
        /** 一键恢复全部高级参数默认值（固定区按钮绑定） */
        resetAll: _handleAdvancedResetAll,
        /** 高级配置组元数据（默认值一致性回归读取） */
        groups: _ADVANCED_GROUPS,
    };
})(window.RamariaSettingsNS);
