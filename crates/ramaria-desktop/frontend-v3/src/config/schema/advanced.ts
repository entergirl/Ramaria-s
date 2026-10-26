/**
 * src/config/schema/advanced.ts — 进阶设置（8 类 / 21 组）的配置键声明
 *
 * 职责:
 * - 声明进阶页全部界面键：检索与索引、模型生成、记忆管线、注入与装配、人格与知识、
 *   主动对话、行为与规则、系统
 *
 * 设计特点:
 * - 每组一张卡、独立保存与恢复默认；组内按用途分小节（`blocks`），不增加目录项
 * - 数值上下界沿用配置基线口径，不在此处推导业务规则
 * - 关闭类开关一律写明回退行为（关闭后走既有路径），避免误以为"无效果"
 */

import {
  type SettingGroup,
  field,
  num,
  order,
  select,
  toggle,
  whitelist,
} from './define'

/** 进阶页配置组（顺序由 `settingsSections` 编排） */
export const ADVANCED_GROUPS: SettingGroup[] = [
  {
    id: 'model-generation',
    title: '生成参数',
    description: '对话模型的采样上限与角色注入位置；与基础页的后端接入口径分开保存。',
    columns: 3,
    fields: [
      num(['backend', 'temperature'], '生成温度', { min: 0, max: 2, step: 0.1 }),
      num(['backend', 'max_tokens'], '最大输出 token', { min: 256, step: 256 }),
      field(['backend', 'role_immersion'], '角色沉浸注入位置', 'select', {
        options: [
          { value: 'off', label: '不注入' },
          { value: 'system', label: '系统提示末尾' },
          { value: 'user', label: '首条用户消息末尾' },
        ],
        hint: '仅 DeepSeek 生效。',
        layout: 'auto',
      }),
    ],
  },
  {
    id: 'retrieval',
    title: '检索参数',
    description: '摘要路的条数、融合权重与 Persona-Aware 过滤；缺通道不惩罚。',
    fields: [
      num(['retrieval', 'l0_window_size'], 'L0 滑动窗口', { min: 1, unit: '条' }),
      num(['retrieval', 'l0_retrieve_top_k'], 'L0 检索条数', { min: 0, unit: '条' }),
      num(['retrieval', 'l1_retrieve_top_k'], 'L1 检索条数', { min: 0, unit: '条' }),
      num(['retrieval', 'l2_retrieve_top_k'], 'L2 检索条数', { min: 0, unit: '条' }),
      num(['retrieval', 'similarity_threshold'], '相似度阈值', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['retrieval', 'rrf_k'], 'RRF 平滑系数 k', { min: 1 }),
      toggle(['retrieval', 'enable_vector'], '向量通道', '关闭后仅 BM25 + 图谱 + 关键词。'),
      toggle(['retrieval', 'enable_graph'], '图谱通道', '关闭后回退 BM25 + 向量 + 关键词。'),
      toggle(['retrieval', 'enable_keyword_channel'], '关键词镜像通道', undefined),
      toggle(
        ['retrieval', 'enable_keyword_fuzzy'],
        '关键词语义层',
        '关闭后词条不嵌入，退化为字面 + 别名归一。',
      ),
      num(['retrieval', 'bm25_weight'], 'BM25 通道权重', { min: 0, step: 0.1 }),
      num(['retrieval', 'graph_weight'], '图谱通道权重', { min: 0, step: 0.1 }),
      num(['retrieval', 'keyword_weight'], '关键词通道权重', { min: 0, step: 0.1 }),
      num(['retrieval', 'retrieval_weight_l2'], 'L2 排序权重', { min: 0, step: 0.1 }),
      num(['retrieval', 'retrieval_weight_l1'], 'L1 排序权重', { min: 0, step: 0.1 }),
      toggle(['retrieval', 'narrative_weighted'], '脉络加权注入', '关闭后取最近 N 条。'),
      num(['retrieval', 'narrative_top_k'], '脉络注入条数', { min: 0, unit: '条' }),
      num(['retrieval', 'rag_max_memories'], 'RAG 上下文条数', { min: 0, unit: '条' }),
      num(['retrieval', 'rag_max_summary_chars'], '单条摘要字符上限', { min: 0, unit: '字符' }),
      num(['retrieval', 'rag_share_threshold_user'], 'Persona-Aware user 阈值', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['retrieval', 'rag_share_threshold_char'], 'Persona-Aware 角色阈值', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['retrieval', 'rag_share_threshold_rama'], 'Persona-Aware rama 阈值', {
        min: 0,
        max: 1,
        step: 0.05,
        hint: '0 = 全量。',
      }),
      toggle(['retrieval', 'rag_include_graph_entities'], '上下文含图谱实体', undefined),
    ],
    blocks: [
      {
        title: '条数与融合',
        keys: [
          'retrieval.l0_window_size',
          'retrieval.l0_retrieve_top_k',
          'retrieval.l1_retrieve_top_k',
          'retrieval.l2_retrieve_top_k',
          'retrieval.similarity_threshold',
          'retrieval.rrf_k',
        ],
      },
      {
        title: '通道开关与权重',
        keys: [
          'retrieval.enable_vector',
          'retrieval.enable_graph',
          'retrieval.enable_keyword_channel',
          'retrieval.enable_keyword_fuzzy',
          'retrieval.bm25_weight',
          'retrieval.graph_weight',
          'retrieval.keyword_weight',
        ],
      },
      {
        title: '脉络与摘要路',
        keys: [
          'retrieval.retrieval_weight_l2',
          'retrieval.retrieval_weight_l1',
          'retrieval.narrative_weighted',
          'retrieval.narrative_top_k',
          'retrieval.rag_max_memories',
          'retrieval.rag_max_summary_chars',
          'retrieval.rag_share_threshold_user',
          'retrieval.rag_share_threshold_char',
          'retrieval.rag_share_threshold_rama',
          'retrieval.rag_include_graph_entities',
        ],
      },
    ],
  },
  {
    id: 'index',
    title: '索引与 BM25',
    description: 'BM25 增量与重建节奏、向量补算批量与 L2 加载上限。',
    fields: [
      num(['index', 'bm25_incremental_threshold'], '增量合并阈值', { min: 1, unit: '条' }),
      num(['index', 'bm25_rebuild_interval'], '重建检查间隔', { min: 10, unit: '秒' }),
      num(['index', 'refresh_interval_seconds'], '内存重建最小间隔', {
        min: 0,
        unit: '秒',
        hint: '0 = 不节流；大于 0 时抑制写入密集期的重建风暴。',
      }),
      num(['index', 'embed_batch_size'], '嵌入批大小', { min: 1, unit: '条' }),
      num(['index', 'l2_load_limit'], 'L2 加载上限', {
        min: 0,
        unit: '条',
        hint: '0 = 不限（分页全量加载）。',
      }),
    ],
  },
  {
    id: 'decay',
    title: '记忆衰减',
    description: 'Ebbinghaus 遗忘曲线参数：稳定性系数、访问加成与显著性调制。',
    fields: [
      num(['decay', 's_l0'], 'L0 稳定性系数', { min: 1, hint: '细节信息衰减最快。' }),
      num(['decay', 's_l1'], 'L1 稳定性系数', { min: 1 }),
      num(['decay', 's_l2'], 'L2 稳定性系数', { min: 1 }),
      toggle(['decay', 'enable_access_boost'], '近期访问加成', undefined),
      num(['decay', 'recent_boost_days'], '加成有效天数', { min: 0, unit: '天' }),
      num(['decay', 'recent_boost_floor'], '加成后保留率下限', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['decay', 'salience_multiplier'], '显著性加成系数', {
        min: 0,
        step: 0.1,
      }),
    ],
  },
  {
    id: 'thresholds',
    title: '层触发阈值',
    description: 'L1→L2 与 L2→L3 的计数 / 天数双路径触发条件。',
    fields: [
      num(['thresholds', 'l2_trigger_count'], 'L2 触发条数', { min: 1, unit: '条' }),
      num(['thresholds', 'l2_trigger_days'], 'L2 触发天数', { min: 1, unit: '天' }),
      num(['thresholds', 'l3_trigger_count'], 'L3 触发条数', { min: 1, unit: '条' }),
      num(['thresholds', 'l3_trigger_days'], 'L3 触发天数', { min: 1, unit: '天' }),
      num(['thresholds', 'cluster_delay_ms'], 'L2 簇间请求间隔', {
        min: 0,
        unit: '毫秒',
        hint: '0 = 不等待；线上 API 建议 500~1000。',
      }),
    ],
  },
  {
    id: 'event-extraction',
    title: '事件提取',
    description: 'L1→L2 事件提取的 LLM 参数；事件 JSON 较长，token 预算需大于对话。',
    fields: [
      num(['event_extraction', 'temperature'], '提取温度', { min: 0, max: 2, step: 0.1 }),
      num(['event_extraction', 'max_tokens'], '最大输出 token', { min: 256, step: 256 }),
      num(['event_extraction', 'max_events'], '单簇最大事件数', { min: 1, unit: '条' }),
      toggle(
        ['event_extraction', 'degraded_confidence_enabled'],
        '降级事件动态置信度',
        '关闭后回退固定 0.5。',
      ),
    ],
  },
  {
    id: 'l1-progressive',
    title: '渐进式摘要',
    description: '长会话封存时按段生成多个 L1；关闭后回退整会话摘要。',
    fields: [
      toggle(['l1.progressive', 'enabled'], '启用渐进式摘要', undefined),
      num(['l1.progressive', 'msg_threshold'], '消息数触发阈值', { min: 2, unit: '条' }),
      num(['l1.progressive', 'span_hours'], '时间跨度阈值', { min: 1, unit: '小时' }),
      num(['l1.progressive', 'tail_msg_count'], '单段最大消息数', { min: 1, unit: '条' }),
    ],
  },
  {
    id: 'utt',
    title: 'utt 原文通道',
    description: '原文话语块的切分、检索与注入；原文为最高敏感层，白名单外不注入。',
    fields: [
      toggle(['utt', 'enabled'], '启用原文通道', '关闭后不注入原文片段。'),
      num(['utt', 'theta_gap_minutes'], '切分时间间隙', { min: 1, unit: '分钟' }),
      num(['utt', 'max_msgs_per_block'], '单块最大消息数', { min: 1, unit: '条' }),
      num(['utt', 'retrieve_top_k'], '检索块数 top_k', { min: 0, unit: '块' }),
      num(['utt', 'max_block_chars'], '注入字符预算', { min: 50, unit: '字符' }),
      whitelist(
        ['utt', 'persona_kind_whitelist'],
        '原文白名单（人格类型）',
        [
          { value: 'char', label: '角色' },
          { value: 'anim', label: '动画' },
          { value: 'oc', label: '原创 OC' },
          { value: 'hist', label: '历史人物' },
        ],
        '助手 / 系统类不注入原文。',
      ),
    ],
  },
  {
    id: 'examples',
    title: '示例注入',
    description: 'Few-shot 示例的自学习抽取与兜底注入。',
    fields: [
      toggle(['examples', 'enabled'], '启用示例注入', undefined),
      num(['examples', 'max_examples'], '最大示例条数', { min: 1, unit: '条' }),
    ],
  },
  {
    id: 'bridge',
    title: '会话桥接',
    description: '新会话加载上一会话尾部原文，隔天回来能接上。',
    fields: [
      toggle(['bridge', 'enabled'], '启用桥接', undefined),
      num(['bridge', 'max_chars'], '桥接字符预算', { min: 50, unit: '字符' }),
    ],
  },
  {
    id: 'injection-budget',
    title: '注入预算',
    description: 'RAG 摘要与四层注入共用 token 池；超限按优先级整块丢弃。',
    fields: [
      toggle(['injection_budget', 'enabled'], '启用协调预算', '关闭时走各层独立预算。'),
      num(['injection_budget', 'max_injection_tokens'], '注入总 token 上限', {
        min: 0,
        unit: 'token',
      }),
      num(['injection_budget', 'max_rag_tokens'], 'RAG 独立 token 上限', {
        min: 0,
        unit: 'token',
        hint: '0 = 仅受总池约束。',
      }),
      order(
        ['injection_budget', 'order'],
        '通道保留优先级（高优先在前）',
        [
          { value: 'rag', label: 'RAG 摘要' },
          { value: 'behavior', label: '行为层' },
          { value: 'knowledge', label: '知识层' },
          { value: 'style', label: '表达层' },
          { value: 'memory', label: '脉络层' },
        ],
        '超预算时从列表末尾开始整块丢弃。',
      ),
    ],
  },
  {
    id: 'layer-dedup',
    title: '层间去重',
    description: '同一事实跨层只注入一次，冲突按来源优先级保留。',
    fields: [toggle(['layer_dedup', 'enabled'], '启用层间去重与仲裁', '关闭时走引用级去重。')],
  },
  {
    id: 'inference',
    title: 'L3 推断',
    description: 'Phase B/C 推断、置信度更新、漂移检测与全量校准。',
    fields: [
      num(['inference', 'inferrer', 'temperature'], '推断温度', {
        min: 0,
        max: 2,
        step: 0.1,
      }),
      num(['inference', 'inferrer', 'max_tokens'], '推断输出上限', {
        min: 128,
        step: 128,
        unit: 'token',
      }),
      num(['inference', 'inferrer', 'low_evidence_threshold'], '小样本证据阈值', {
        min: 0,
        step: 0.5,
      }),
      num(['inference', 'inferrer', 'step_max_tokens'], '每步输出上限', {
        min: 128,
        step: 128,
        unit: 'token',
      }),
      num(['inference', 'confidence', 'stability_s'], '置信度稳定性 S', {
        min: 1,
        step: 1,
      }),
      num(['inference', 'confidence', 'min_decay'], '时间衰减保底', {
        min: 0,
        max: 1,
        step: 0.01,
      }),
      num(['inference', 'drift', 'alpha'], '漂移显著性水平', {
        min: 0.001,
        max: 1,
        step: 0.01,
      }),
      num(['inference', 'drift', 'n_permutations'], '置换检验次数', {
        min: 100,
        step: 100,
      }),
      num(['inference', 'calibration', 'round_threshold'], '校准轮次阈值', { min: 1 }),
      num(['inference', 'calibration', 'event_doubling_ratio'], '事件翻倍比例', {
        min: 1,
        step: 0.1,
      }),
      num(['inference', 'calibration', 'diff_alert_ratio'], '差异告警比例', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      toggle(
        ['inference', 'upgrade', 'cross_version_threshold_085'],
        '跨版本簇阈值 0.85',
        '关闭后回退 0.75。',
      ),
      toggle(
        ['inference', 'upgrade', 'cold_start_cross_user_prior'],
        '跨用户冷启动先验',
        undefined,
      ),
      toggle(
        ['inference', 'upgrade', 'drift_restore_real_distribution'],
        '漂移检测恢复真实分布',
        '关闭后漂移检测整体跳过。',
      ),
      toggle(
        ['inference', 'upgrade', 'causal_latency_emotion_trend'],
        '因果时延与情绪走势',
        undefined,
      ),
    ],
    blocks: [
      {
        title: '推断器',
        keys: [
          'inference.inferrer.temperature',
          'inference.inferrer.max_tokens',
          'inference.inferrer.low_evidence_threshold',
          'inference.inferrer.step_max_tokens',
        ],
      },
      {
        title: '置信度',
        keys: ['inference.confidence.stability_s', 'inference.confidence.min_decay'],
      },
      {
        title: '漂移检测',
        keys: ['inference.drift.alpha', 'inference.drift.n_permutations'],
      },
      {
        title: '全量校准',
        keys: [
          'inference.calibration.round_threshold',
          'inference.calibration.event_doubling_ratio',
          'inference.calibration.diff_alert_ratio',
        ],
      },
      {
        title: '画像升级',
        keys: [
          'inference.upgrade.cross_version_threshold_085',
          'inference.upgrade.cold_start_cross_user_prior',
          'inference.upgrade.drift_restore_real_distribution',
          'inference.upgrade.causal_latency_emotion_trend',
        ],
      },
    ],
  },
  {
    id: 'knowledge',
    title: '知识层',
    description: '事件到事实的抽取、判重与注入；总开关默认关闭。',
    fields: [
      toggle(
        ['knowledge', 'auto_fact_detect'],
        '自动事实检测',
        '关闭时与既有版本完全一致。',
      ),
      toggle(['knowledge', 'detector_enabled'], '规则判定器', '关闭后不检索注入。'),
      num(['knowledge', 'retrieve_top_k'], '候选检索条数', {
        min: 0,
        unit: '条',
        hint: '0 = 不按条数截断。',
      }),
      num(['knowledge', 'retrieve_threshold'], '候选检索路由阈值', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['knowledge', 'dedup_cosine_threshold'], '判重余弦阈值', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['knowledge', 'dedup_keyword_min'], '判重关键词下限', { min: 1, unit: '个' }),
      num(['knowledge', 'corroboration_cosine_threshold'], '互证余弦阈值', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['knowledge', 'injection_budget_chars'], '事实卡片注入预算', {
        min: 0,
        unit: '字符',
      }),
      num(['knowledge', 'volatile_halflife_days'], '动态事实半衰期', {
        min: 1,
        unit: '天',
      }),
    ],
  },
  {
    id: 'style',
    title: '说话风格',
    description: '五维风格统计、显著性检验与自动规则生成；关闭后提示词不含自动风格规则。',
    fields: [
      toggle(['style', 'enabled'], '风格统计总开关', undefined),
      toggle(['style', 'auto_translate'], 'LLM 离线翻译增强', '关闭后仅模板拼接。'),
      toggle(['style', 'sample_fallback'], '小样本原文样例兜底', undefined),
      toggle(['style', 'keyword_dict'], '关键词体系衔接', undefined),
      num(['style', 'min_sample_count'], '样本量阈值', { min: 1, unit: '条' }),
      num(['style', 'top_n'], '口癖 / 话题词 Top-N', { min: 1, unit: '个' }),
      num(['style', 'relative_boost_ratio'], '相对超频比阈值', {
        min: 0,
        step: 0.1,
      }),
      num(['style', 'min_frequency'], '显著项最小频次', { min: 1, unit: '次' }),
      num(['style', 'z_critical'], 'z 临界值', { min: 0, step: 0.1 }),
    ],
  },
  {
    id: 'proactive-judge',
    title: '主动对话 · 判据与选题',
    description: 'AI 判据与候选选题的取舍口径；非排障场景建议保持默认值。',
    fields: [
      toggle(['proactive', 'judge_enabled'], '启用 AI 判据', '关闭后由算法打分直接决策。'),
      num(['proactive', 'judge_interval_hours'], '判据节流间隔', { min: 0, unit: '小时' }),
      num(['proactive', 'confidence_floor'], '事件置信度门槛', {
        min: 0,
        max: 1,
        step: 0.1,
      }),
      num(['proactive', 'light_touch_weight'], '轻触达兜底权重', {
        min: 0,
        max: 1,
        step: 0.1,
      }),
      num(['proactive', 'event_salience_threshold'], '事件显著性门槛', {
        min: 0,
        max: 1,
        step: 0.1,
      }),
      num(['proactive', 'event_window_days'], '事件时间窗', { min: 1, unit: '天' }),
      num(['proactive', 'unresolved_valence_threshold'], '未了结负效价阈值', {
        min: -1,
        max: 0,
        step: 0.1,
      }),
      num(['proactive', 'follow_up_days'], '跟进时机', { min: 1, unit: '天' }),
      num(['proactive', 'topic_cooldown_hours'], '选题去重冷却', { min: 0, unit: '小时' }),
      num(['proactive', 'silence_backoff_days'], '沉默退避阈值', { min: 1, unit: '天' }),
      num(['proactive', 'startup_grace_days'], '首次启用宽限期', { min: 0, unit: '天' }),
      num(['proactive', 'valence_weight'], '效价入权强度', {
        min: 0,
        max: 1,
        step: 0.1,
      }),
    ],
  },
  {
    id: 'proactive-rhythm',
    title: '主动对话 · 时段建模',
    description: '按用户活跃时段与人格节律做软加权；样本不足时自动退化放行。',
    fields: [
      num(['proactive', 'active_hours_weight'], '活跃时段加权强度', {
        min: 0,
        max: 1,
        step: 0.1,
      }),
      num(['proactive', 'active_hours_window_days'], '活跃时段统计窗口', {
        min: 1,
        unit: '天',
      }),
      num(['proactive', 'active_hours_min_samples'], '时段建模样本门槛', { min: 0, unit: '条' }),
      num(['proactive', 'rhythm_weight'], '人格节律加权强度', {
        min: 0,
        max: 1,
        step: 0.1,
        hint: '0 = 关闭人格节律。',
      }),
      num(['proactive', 'rhythm_window_days'], '人格活跃曲线窗口', { min: 1, unit: '天' }),
      num(['proactive', 'rhythm_min_samples'], '人格节律样本门槛', { min: 0, unit: '条' }),
    ],
  },
  {
    id: 'proactive-greeting',
    title: '主动对话 · 问候与近期状态',
    description: '刚睡醒时段的问候，以及近期心情对回复语气的影响。',
    fields: [
      toggle(['proactive', 'greeting_enabled'], '早安问候', undefined),
      num(['proactive', 'greeting_window_minutes'], '问候时机窗口', {
        min: 0,
        unit: '分钟',
      }),
      toggle(['proactive', 'recent_state_enabled'], '近期状态注入', undefined),
      num(['proactive', 'recent_state_window_days'], '近期状态统计窗口', {
        min: 1,
        unit: '天',
      }),
    ],
  },
  {
    id: 'behavior-feedback',
    title: '行为规则与反馈',
    description: '行为层聚类与路由参数，以及弱反馈采集与复审阈值。',
    fields: [
      toggle(['behavior', 'enabled'], '行为层总开关', '关闭后不学习、不路由。'),
      num(['behavior', 'theta_nb'], '聚类邻域阈值 θ_nb', { min: 0, max: 1, step: 0.05 }),
      num(['behavior', 'min_cluster_size'], '核心样本最少邻居数', { min: 1 }),
      num(['behavior', 'theta_join'], '增量归簇阈值 θ_join', { min: 0, max: 1, step: 0.05 }),
      num(['behavior', 'beta1'], '反应通道权重 β1', { min: 0, max: 1, step: 0.05 }),
      num(['behavior', 'beta2'], '情境通道权重 β2', { min: 0, max: 1, step: 0.05 }),
      num(['behavior', 'theta_route'], '情境路由阈值 θ_route', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['behavior', 'gamma'], '路由评分 cos 权重 γ', { min: 0, max: 1, step: 0.05 }),
      num(['behavior', 'top_n'], '路由合并上限', { min: 1, unit: '条' }),
      num(['behavior', 'min_evidence'], '规则证据量门槛', { min: 1, unit: '条' }),
      num(['behavior', 'min_n_eff'], '有效样本量门槛', { min: 1 }),
      num(['behavior', 'valence_std_limit'], '簇内效价标准差上限', {
        min: 0,
        step: 0.1,
      }),
      num(['behavior', 'max_outlier_ratio'], '聚类孤立点比例上限', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['behavior', 'pending_expire_days'], '待定池过期', { min: 1, unit: '天' }),
      num(['behavior', 'evidence_decay_threshold'], '证据衰减保留率下限', {
        min: 0,
        max: 1,
        step: 0.05,
      }),
      num(['behavior', 'recent_days'], '近期事件加权窗口', { min: 1, unit: '天' }),
      toggle(['feedback', 'enabled'], '弱反馈采集总开关', undefined),
      toggle(
        ['feedback', 'auto_apply_weak_feedback'],
        '弱反馈自动应用',
        '关闭时只写审计日志，规则与画像零自动修改。',
      ),
      num(['feedback', 'correction_window_ms'], '纠正检测窗口', {
        min: 0,
        unit: '毫秒',
      }),
      num(['feedback', 'continue_window_ms'], '继续发言检测窗口', {
        min: 0,
        unit: '毫秒',
      }),
      num(['feedback', 'dedup_window_ms'], '重复反馈去重窗口', {
        min: 0,
        unit: '毫秒',
      }),
      num(['feedback', 's3_trend_window'], 'S3 趋势窗口', { min: 1, unit: '次' }),
      num(['feedback', 's3_continue_trigger'], '复审触发连续次数', { min: 1, unit: '次' }),
      num(['feedback', 's3_stop_trigger'], '复审解除连续次数', { min: 1, unit: '次' }),
    ],
    blocks: [
      {
        title: '行为层',
        keys: [
          'behavior.enabled',
          'behavior.theta_nb',
          'behavior.min_cluster_size',
          'behavior.theta_join',
          'behavior.beta1',
          'behavior.beta2',
          'behavior.theta_route',
          'behavior.gamma',
          'behavior.top_n',
          'behavior.min_evidence',
          'behavior.min_n_eff',
          'behavior.valence_std_limit',
          'behavior.max_outlier_ratio',
          'behavior.pending_expire_days',
          'behavior.evidence_decay_threshold',
          'behavior.recent_days',
        ],
      },
      {
        title: '弱反馈环',
        keys: [
          'feedback.enabled',
          'feedback.auto_apply_weak_feedback',
          'feedback.correction_window_ms',
          'feedback.continue_window_ms',
          'feedback.dedup_window_ms',
          'feedback.s3_trend_window',
          'feedback.s3_continue_trigger',
          'feedback.s3_stop_trigger',
        ],
      },
    ],
  },
  {
    id: 'cache',
    title: '缓存',
    description: '精确缓存容量与 L2 事件指纹去重；关闭缓存后每次调用都走模型。',
    fields: [
      toggle(['cache', 'enabled'], '精确缓存总开关', undefined),
      num(['cache', 'max_entries'], '缓存条目上限', { min: 1, unit: '条' }),
      select(
        ['cache', 'eviction'],
        '淘汰策略',
        [
          { value: 'lru', label: 'LRU（最近最少使用）' },
          { value: 'fifo', label: 'FIFO（先入先出）' },
        ],
      ),
      toggle(['cache', 'l2_fingerprint_enabled'], 'L2 聚类去重指纹', undefined),
      num(['cache', 'l2_similarity_threshold'], '事件相似度阈值', {
        min: 0,
        max: 1,
        step: 0.01,
      }),
      num(['cache', 'l2_recent_events_limit'], '去重比对条数', { min: 1, unit: '条' }),
    ],
  },
  {
    id: 'logging',
    title: '日志',
    description: '日志不记录消息原文、模型原始响应与密钥。',
    fields: [
      toggle(
        ['logging', 'log_full_prompt'],
        '记录完整 Prompt',
        '预留项：当前未接线，开启后也不会写入完整提示词；启用前会先更新隐私说明。',
      ),
    ],
  },
]
