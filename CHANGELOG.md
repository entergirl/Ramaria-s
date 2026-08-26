# 变更日志

本文档记录 Ramaria Rust 版的所有显著变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

---

## [1.6.0] - 2026-08-26

### 核心特性

#### 知识层（v3.1 §5，知识深化）

回答"ta 的事都能答上"——从记忆事件自动抽取结构化事实，分层、版本链仲裁、按需注入：

- **事实抽取**（`ramaria-memory/src/fact/`）：LLM 从事件 paraphrase+attitude+keywords 抽取事实（标注 ProfileField）+ 规则兜底（LLM 不可用时关键词/模板规则）；触发条件事件 confidence ≥ 0.6 且客观/混合；主观事件额外抽取隐含偏好事实（conf=0.5 入 candidate，互证后提升 active）。
- **判重**：同 field 语义余弦 ≥ 0.85 **且** 关键词交集 ≥ 1 → 不入库（双条件）。
- **分层与时效**：稳定（需互证或 manual 覆盖）/ 动态（新覆盖旧留版本链，随事件衰减）/ 历史（只追加）。
- **版本链仲裁**：manual > 多事件互证 > 单事件；互证 = ≥2 独立事件 + 语义余弦 ≥ 0.7 + valence 一致。
- **规则判定器检索注入**（`render_knowledge_block`）：事实类疑问词 / 话题关键词命中 / 显式指代，零新增 LLM 调用；同 field 召回 + 向量检索（时效加权）；不命中/关闭 → 不注入（回退 v1.5 语义等价）。
- **`[knowledge]` 配置组**：`auto_fact_detect=false` 默认关闭、判定器开关、判重/互证阈值、注入预算。
- **CLI / UI**：`ramaria fact list/show`（按 `--persona/--field` 过滤 + `--json` 信封 + 版本链；**双端均无 delete**）+ 记忆页「知识」只读卡片（ProfileField 分组 + 历史版本折叠；`node --test` 前端纯逻辑测试引入）。

#### 画像升级（v3.1 §3）

"画像更准"：

- 跨版本匹配阈值统一 **0.85**（`match_clusters_cross_version` 0.75 → 0.85）。
- 冷启动先验校准：A5 收缩先验改用系统内已有人格画像的**跨用户经验分布**（首个 persona 回退统一默认）。
- 降级事件置信度 `min(0.59, 0.35 + 0.02 × n_l1)` 封顶 0.59、恒 tentative。
- Phase C 漂移检测**真实实现**：从 `persona_cluster_snapshots` samples JSON 恢复真实旧分布（替换硬编码 0/0.5）。
- 以上均有独立配置开关，关闭后回退 v1.5 行为。

#### 边界与隐私（v3.1 §11/§12）

- 降级路径全覆盖：embedding 不可用（在线 utt 降级 L1/行为路由关键词/知识同 field 召回，离线 A4 跳过 + 行为层纯关键词聚类 β=0）、LLM 不可用（知识规则兜底/行为跳过该簇/L1 保留重试）、冷启动、数据稀疏——各路径均不阻塞主流程。
- 原文通道白名单 + 检索/注入严格按 persona_uid 隔离（跨 persona 不可见）。
- 日志脱敏：不记 L1 摘要全文（记 id/长度）、QQ 号、LLM 原始响应全文。

#### 探针自动评分（T2）

- `probe evaluate`：事实维 golden（embedding 余弦 + 关键词命中加权）+ 语气维 LLM-as-judge（本地 LM Studio，rubric 1~5，温度 0）。
- `probe report`：档位对比表 + 定稿建议（markdown/JSON 双形态）；人工抽检 10%~20% 校准。
- 知识层误报/漏报评估（目标漏报 <10%）。

#### 启动前置与数据（M0）

- CLI 一致性四项修复：`RAMARIA_DB_PATH` env、config.toml 经 `ConfigSyncService` 加载、embedding provider（native）、probe persona 按"对方"语义选择。
- "我方/对方"数据库对齐（D-V16-011）：`build_persona_uid` 增加我方分支（self → `user-*`/kind=user，对方仍 `char-*`）+ `import --side self|other|both`（默认 both，跳过侧消息不入库、该侧 persona 不创建）+ 桌面导入面板选项。
- 向量通道接线（D-V16-013）：L1/L2 embedding 真实入索引（`parse_doc_label` 前缀解析 + `CachedVectorIndex` 容量策略修正）；`enable_vector` 真实生效。
- 探针性能：GPU 向量推理（candle CUDA，`[embedding].device`）+ 内容级去重（`UttBuilder` embedding_cache，幂等挂载）+ 行为层近期事件加权修正（recency_factor 真实生效，D-V16-007）。
- `native.rs` `ensure_loaded` dimension 维度同步（构造-下载-validate 不再报"维度不匹配"）。

#### 破坏性变更

- **migrations 合并为单基线 `20260815_v1.6_schema.sql`**（v1.0 + v1.4/v1.5 增量 + v1.6 新结构）——旧库无法自动迁移，**需重建库**（备份 → 重建 → 重新导入 → 关键数据核对）；`persona_facts` 以版本化结构（status/tier/version_of/confidence/keyword_hint）直建。
- "我方" persona kind 修正（self → user kind），白名单过滤天然排除我方。
- 行为变更（非破坏）：画像阈值 0.75→0.85、降级置信度封顶 0.59、冷启动先验、向量通道激活——画像/检索输出可能变化，回归已更新。

### 说明

- utt 参数定稿（θ_gap/条数上限/top_k）与 D-P 聚类参数摸底档位实验**延后至 v1.7**（DeepSeek 平台无 `seed` 不保证复现，复跑一致性不可达）；v1.6 保留已完成前置（近期加权、GPU 推理、探针环境验证）。

---

## [1.5.0] - 2026-08-15

### 核心特性

#### 行为层（L3 行为模型，v3.1 §4）——情境-反应规则全链路

persona 的反应模式自动复现：从记忆事件中学习"在什么情境下 ta 会怎么反应"，生成行为规则并在对话中自动命中注入。新增 `ramaria-memory/src/behavior/` 模块（clustering / sentiment / rule_gen / routing / incremental）与 `behavior_rules`/`feedback_log` 表：

- **情境-反应聚类（D2）**：事件 → 情境-反应对样本，双通道向量化（反应通道 `embedding(paraphrase⊕attitude)`、情境通道 `embedding(关键词拼接)`），三路融合相似度 `sim = β1·cos(反应) + β2·cos(情境) + (1−β1−β2)·Jaccard(关键词)`（β1=0.4/β2=0.3/关键词 0.3，缺通道权重归一化）；密度聚类（邻域 θ_nb=0.5、核心样本邻居 ≥ min_cluster_size=3、密度可达链式传播、边界软分配、孤立点不入簇）；失败模式检查（孤立点比例 > 60% 下调 θ_nb 重试）；簇提炼（关键词 Top-N / 簇中心 / valence 加权均值与标准差 / presentation 分布 / situation_strength / 时间跨度 / 簇质量）。
- **规则生成（D4）**：每簇 → LLM 翻译规则文本（JSON `{reaction, avoid}`，引用簇内代表事件 attitude 作示例）；翻译后**极性一致性校验**（内置中文情感词典提取规则文本极性，与簇内加权 valence 符号比对，不一致重试 1 次仍不一致 → 降级候选规则仅参数注入）；avoid 列表与低 valence 事件相关性校验；质控双门槛（证据量 ≥ 5 / n_eff ≥ 5 / valence 方差 ≤ 0.5）；参数化（情感强度 = 加权 valence、表达倾向 = presentation 分布）；近期事件加权；Auto 规则自动生效（无人工参与）。
- **情境路由（D5）**：查询构造（最近 5 条消息 → 查询向量 q + 话题词 Top-10）；候选评分 `score = γ·max(0, cos(q, 簇中心)) + (1−γ)·Jaccard(查询侧)`（γ=0.7，cos clip [0,1]，**查询侧** Jaccard 避免偏袒窄规则）；阈值 θ_route=0.6 全低于 → 不注入（静默降级，等同 v1.4）；Top 1~3 排序合并（主规则完整注入、次规则仅合并 avoid 与互补 params、valence 方向矛盾丢弃次规则）。
- **增量更新（D6）**：会话封存时新事件归入最近簇（≥ θ_join=0.7，滚动更新簇统计与规则参数微调）；未归入 → 待定池（内聚成簇 / 30 天未成簇低置信标记）；旧规则证据按 Ebbinghaus 衰减、低于阈值降级/失效；系统性变化复用漂移检测（Wasserstein + 置换检验）触发规则重构。
- **规则管理后端 + CLI（D7）**：`ramaria rule list/show/import/edit/enable/disable/delete/evidence` 8 动词子命令（遵循 §2.9 词表与 M1 `--json`/`--yes` 约定）；手工导入 JSON（宽松 situation 解析、空情境/空规则拒绝）；`evidence` 展示规则 → 事件 → 原文摘要溯源链（只含结构化字段，原文不落日志）。规则管理前端 UI 延后（D-V15-004 决策）。
- **反馈环 S1（H1，并入 D7）**：`feedback_log` 表（S1=edit/disable weight=1.0，detail 编辑前后快照；S2/S3 类型预置，v1.7 复用只增不删）；edit/disable 写 S1 强信号；edit 后规则转 Manual；**Manual 强锚点**（learn 时 Manual 规则以 salience=1.0 锚点样本注入聚类偏移簇中心）。

#### 驱动环接线（F，v3.1 §8）——行为控制块注入

填充 v1.4 预留的 `render_behavior_block` 空槽位：路由命中时渲染【角色（行为层）】段落（`## 行为规则` 小节：关键词 + reaction + params 数值行 + avoid 行，规则文本为主参数为辅）；注入优先级行为 > 知识 > 表达 > 脉络（§8.1）；行为块预算受 §8.3 约束（`behavior_block_max_chars` 默认 400，超限保前部截断 + 最小预算 24 防御残缺段落）。新增 `[behavior]` 配置组（enabled / θ_route / γ / top_n / 聚类与质控参数）全链路传播；**行为层关闭或未命中时 prompt 与 v1.4 语义等价**（回归断言锁定，`PROMPT_TEMPLATE_VERSION` 递增至 `20260814-v1.5.1` 使旧缓存失效）。

#### 三层生成缓存（C，不含语义缓存）

重跑/重试/失败恢复导入与生成管线时不重复花费 API 账单，L2 不做无意义重复聚类：

- **LLM 响应精确缓存**：`llm_response_cache` 表（key 主键 = sha256(model_id + template_version + prompt)，只存响应不存原文输入）；`ChatRequest` 新增 `template_version` 字段（prompt 模板版本常量）；`ProviderBase` 全链路查询-写入（命中记 `cache_hit=true` 直接复用、未命中走 LLM 成功后写入、查询/写入失败记 warn 降级、template_version 为空跳过）；表容量自淘汰（LRU/FIFO 策略）。
- **L2 聚类去重指纹**：`l2_cluster_fingerprints` 表记录"已聚类且无产出"的 L1 集合指纹（SHA-256 集合指纹，顺序无关）；同集合跳过不重复聚类；新事件与已有事件相似度去重（字符 bigram / 关键词 Jaccard 取大，阈值 0.95，最近 200 条比对）。
- 新增 `[cache]` 配置组（enabled 默认开启 / max_entries=10000 / eviction / l2_fingerprint_enabled / 相似度阈值），关闭后行为回退 v1.4（回归断言锁定）。

#### 上下文感知生成（B2，v3.1 §6.3）

L1 摘要生成质量提升：生成块 N 时注入上一块上文——上一块消息数 ≤ 阈值（默认 20）→ 注入 L0 原文；长块 → 注入上一 L1 摘要 + 结构化线索（evidence_notes）；**只注入最近 1 块（不链式）**。Prompt 增强"判断当前对话是否延续上一话题；延续则在摘要与线索中体现延续性，无关则独立摘要"；输出含 `continuation`（延续/转折/无关）。缺失槽位降级：cause 缺失置空不阻塞、结构化线索缺失降级空数组记 warn、无上一块/无 utt 块 → 与 v1.4 行为一致（独立摘要断言锁定）。

#### utt 切分单边合并方向变更（D-V15-014）

单边块（块内消息全部来自同一发言侧）并入方向由「并入前块优先」改为「按时间间隔更短的一侧」——比较单边块首条与前块末条间隔、后块首条与单边块末条间隔，取短侧（首块仅后侧、末块仅前侧）；合并可突破 θ_gap 与条数上限，收敛循环，仅剩一块保留。修复异步对话（跨夜回复）中提问型独白与回复的问答配对断裂。仅作用于导入/重建路径的离线切分，实时对话不经过切分；utt 块为派生数据，`blocks rebuild` 重建幂等（非破坏）。

#### 导入进度 ETA（I）

导入后台任务期间前台显示「已完成 x/y · 预计剩余 N 分钟」：后端按阶段（L1/L2/L3）分别统计——L1 调用数可预知（= session × persona）、L2 聚类簇在线估算（已聚簇数/预计簇数）、L3 固定阶段数；`import-progress` 事件 payload 增加各阶段预计总量字段；EMA 平滑分层单次耗时，剩余时间 = Σ(各阶段剩余量 × EMA 单次耗时)；后端统计不可得时前端回退线性估算（降级路径）。

#### CLI 自动化友好改造与命名规范化（M1，D-V15-005/006/007/011）

- **全局 `--json`**：统一信封 `{"ok":true,"data":…}` / `{"ok":false,"error":{"code":…,"message":"…"}}`（D-V15-011），stdout 只输出数据、状态/提示/警告走 stderr，新增 `--quiet`（抑制 stderr 提示）。
- **`ask --json` 修复**：`StreamEvent` 实现 `Serialize`，事件流 `{"type":"delta|done|error",…}`；`--no-stream` 聚合为单个 `done`（reply/session_id/total_chars）。
- **`--yes` 全覆盖 + 非 TTY 不挂起**：所有确认点（session delete / import / rule delete 等）支持自动确认；非 TTY 且无 `--yes` 直接失败并提示；破坏性操作统一 `--force` 双保险。
- **exit code 约定**：0 成功 / 2 参数错（clap）/ 3 LLM 或后端不可用 / 4 业务校验失败；`--json` 模式时间戳统一 ISO-8601 UTC。
- **增量命令**：`persona list`（uid/名称/kind/来源/状态，分页）、`status`（应用状态/配置摘要/DB 路径，agent 探活）、`import qq --dry-run`（解析预览不写库）；memory/session/config 查询类全部补 `--json`；`memory` 默认 persona 修正（`user-0001` → `rama-0001`，缺陷修复）；`--output` 统一支持 `-` = stdout。
- **命名与语法规范**：命令结构 `ramaria <对象> <动作> [位置参数] [选项]`；`memory <层>` 层级别名 `l1↔summary`/`l2↔events`/`l3↔profile`（双支持 + 纠错提示）；`utt` → `blocks`（保留 alias）；`probe dataset` → `probe build`（保留 alias）；`ramaria help` 分组（对话/记忆/数据/管理/高级）带示例；错误提示带纠错。

#### 探针 CLI 骨架（M2 T1）

`ramaria probe build/run`：从导入数据自动构建测试集（2 维「语气模仿/事实记忆」× 10 题，seed 固定可复跑，无真实数据时 fixture 兜底）+ 按参数档位批量跑对话管线（结构化输出 档位 → 输出 → 指标）。工具链代码完成；**档位实验（utt 参数定稿 + D-P 聚类参数摸底）延后至 v1.6**（D-V15-013：CLI 未接入 embedding provider + probe persona 选择缺陷，见 `docs/dev-1.6/备忘.md`），utt 参数维持 v3.1 初值（θ_gap=30 / 条数上限 40 / top_k=3）带"待实证"标注。

#### 设置页视觉整改（U）与前端加固

- 对照 `frontend/css/tokens.css` 粉蓝双色设计系统与 `desktop-design.html` 整改基础/高级两级设置页（视觉差异清单见 `docs/dev-1.5/v1.5-m6-settings-visual-checklist.md`）：Tab 由边框式按钮改为分段控件（gray-100 底 + 无边框 + 激活白底 shadow-sm）、`.settings-risk-banner` 改品牌粉语义色、checkbox accent-color 改 `--color-primary`、全部硬编码色值清除；设置项分组与配置链路零改动（功能回归红线）。
- 交互增强（负责人追加）：数值输入框默认值浅色显示（非默认值自动转深色）；默认开的选项预置 `checked`；高级设置页底部新增「⚙️ 恢复默认」独立栏（点击直接执行，无弹窗）；导入页顶部三步骤窗口删除 640px 断点竖排规则、改常驻 `flex-wrap: wrap`（缩窄保持横排）。

### 修复

- **CSP 违规（负责人反馈）**：`frontend/index.html` meta CSP 严格模式（`style-src 'self'`）阻止 6 处 HTML 字符串内嵌 `style="..."` 属性——静态样式改类（`.settings-update-detail`/`.settings-actions-tight`），动态宽度/显隐改渲染后 CSSOM 设置（不受 style-src 限制）；CSSOM 类操作不受 CSP 限制保留不动。
- **安全审查 4 项**（security-review）：① HIGH 检查更新结果（GitHub API 远程内容）未转义拼 innerHTML → 模块级 `_escapeHtml`（含属性值转义）先行转义再渲染；② MEDIUM `tauri.conf.json` CSP 含 `'unsafe-inline'` 与 meta 严格策略不一致 → 收敛为与 `index.html` meta 完全一致（connect-src 补 `http://ipc.localhost https://ipc.localhost`）；③ LOW `_advSetValue` 缺失中间路径对象抛 TypeError → 写前自动补建；④ LOW `memory.js` 证据链加载失败分支 `err.message` 未转义 → `_escapeHtml` 转义。
- 既有缺陷：`memory` 默认 persona `user-0001` → `rama-0001`（查询默认对象指向错误对象的缺陷修复）。

### 破坏性变更

- **CLI 命名（非破坏，alias 兼容）**：`utt` → `blocks`、`probe dataset` → `probe build`，旧命令名继续可用。
- **stdout/stderr 分离（脚本注意）**：状态/提示/警告消息改走 stderr，stdout 仅保留数据——既有文本输出的数据部分不变，仅提示位置变化；依赖"stdout 混杂状态行"的脚本需调整。
- **`ask --json` 输出格式修复**：由 Rust Debug 格式（非合法 JSON）修复为真 JSON 事件流——旧格式本不可解析，按修复处理。
- **`memory` 默认 persona 修正**：`user-0001` 硬编码改为 `rama-0001`——查询默认对象变化，属缺陷修复。
- **utt 切分单边合并方向（数据重建注意，非破坏）**：单边块并入方向由「前块优先」改为「时间短侧」——既有 utt 块边界随之变化，需 `blocks rebuild` 生效，对应 L1 摘要重新生成（LLM 成本）；utt 块为派生数据，重建幂等（按 start_msg_id 去重）。
- **无数据库破坏性变更**：`behavior_rules`/`feedback_log`/`llm_response_cache`/`l2_cluster_fingerprints` 均为新增表（增量 migration），既有表结构不变。

### 说明

- 探针档位实验（utt 参数定稿 T-V15-2-003 + D-P 聚类参数摸底 T-V15-5-003）延后至 v1.6（D-V15-013）：CLI 未接入 embedding provider（检索退化 BM25）+ `probe build` 自动 persona 选择未考虑 role 分布；阻塞项修复见 `docs/dev-1.6/备忘.md`；`v1.5-probe-report.md` 保持草稿。
- 规则管理前端 UI 延后（D-V15-004）：v1.5 提供后端 API + CLI 管理（`ramaria rule`）。
- 生成缓存不含语义缓存（用户决策 2026-08-08，D-V15-008）。
- 反馈环 S2/S3 弱反馈检测与风格特征统计延后至 v1.7（H2）；知识层延后至 v1.6。
- 待项目负责人验收：全量 `cargo test --workspace`；真实 LLM Smoke Test（行为规则自动生成并生效、精确缓存命中 + API 账单对比、导入 ETA 一致性、B2 延续/转折体感）；设置页手动视觉验收；CSP 修复验证（进入设置页控制台无 `style-src` 违规报错）。

---

## [1.4.0] - 2026-08-08

### 核心特性

#### utt 话语块（L0 原文表达层）

新增 `utt_blocks` 表与 `ramaria-memory/src/utt/` 模块，实现原文话语块全链路：切分器按时间间隙（θ_gap=30 分钟）与条数上限（40 条）切分会话消息，块内必须含目标 persona 发言，单边对话块自动与相邻块合并、无发言块丢弃；构建器支持全量重建（按 start_msg_id 幂等去重）与增量构建（会话封存钩子，只处理未切分消息）；块 embedding 写入 BruteForceIndex 的 layer='l0' 新通道，检索向量优先、embedding 不可用时降级 BM25 子串匹配，persona_uid 严格隔离。对话时经【原文片段】段落整块注入（超预算按相似度从低到高丢整块）。原文通道受 `persona_kind_whitelist` 白名单约束（默认仅角色类 persona 开启），助手/系统类不注入、行为与 v1.3 完全一致，原文内容不写日志。相关测试约 80 个。

#### examples 自学习激活（Few-shot 风格兜底）

`persona_examples` 写侧激活：会话封存时纯规则抽取"对方 → 你"回复对（过滤图片/过短/系统消息/重复），经 `save_example` 入库候选池（查重幂等）。注入侧激活 `example_selector.rs` 多维评分（话题相关/情绪/长度）轮换选择，替代 v1.3 静态 `selected=1` 查询；记忆检索未命中（`memory_context=None`）时注入 examples 作风格兜底，命中时不重复注入。相关测试约 45 个。

#### evidence_notes 结构化线索

`memory_l1.evidence_notes` 从字符串数组升级为结构化对象数组 `[{text, time?, who?, cause?}]`（text 必填 ≥ 5 字符，time/who/cause 可选，1~3 条）。L1 Summarizer Prompt 升级输出对象数组并附槽位说明；后处理校验可选槽位 trim + 空白归一为 None，过短条目不记原文日志（隐私红线）。存量数据一次性迁移（旧字符串数组落 `text` 槽位，迁移前备份原值，无运行时兼容解析）。L2 事件提取注入 cause 槽位因果线索段落（仅供背景参考）；TopicBatcher 语义增强输入适配新结构（summary + evidence_notes + keywords）。相关测试约 30 个。

#### 会话桥接与生命周期

新会话创建时取最近一个已关闭会话的最后一个 utt 块（无块降级取末 5 条原文，仍无跳过），以 `[时间] 角色: 内容…` 格式注入【桥接（上一会话尾部）】段落（只取最近一个不链式，`bridge_enabled` 开关默认开启，预算从头部截断保最近，内容受原文白名单约束）。空闲自动保存时长可配置：设置页【会话】区块滑动块 5~60 分钟（滑到尽头切换自定义输入），保存后热更新空闲检测线程（`Arc<AtomicU32>`，无需重启）。空闲检测遍历 DB 全部活跃会话，孤儿会话不再遗留。相关测试约 25 个。

#### 驱动环骨架与四层注入

CRISPE 模板精简对齐 v3.1 §8.2 四层结构（角色行为层/说话风格表达层/知识层/记忆脉络层），段落映射表文档化。新增 `prompt/layers.rs` 统一注入块（`LayerKind`/`InjectionBlock`）与预算分配器（脉络独立预算 ≤ 30%，默认 600 字符；裁剪顺序：原文块按相似度 → 桥接截头部 → 相关记忆句子边界 → 脉络保最近）；行为/知识注入块空实现预留（v1.5/v1.6 填充）。`[utt]/[examples]/[bridge]` 配置组经 `RamariaConfig` 全链路传播，开关关闭后行为回退 v1.3（回归断言锁定）。相关测试约 35 个。

#### 设置功能完善

打通 config.toml 与 DB 双写同步链路（`ramaria-app/src/config_sync.rs`）：启动读取两处并做一致性校验（不一致以文件为准并告警），config.toml 缺失时生成含全部默认值的模板，设置页修改经统一写入口同时落文件与表，API key 仍走 keychain 不入文件，单侧写失败降级不阻塞。设置页重构为基础/高级两级 Tab：基础（LLM 后端/嵌入模型/记忆注入开关/会话/隐私/数据目录），高级（检索/衰减/阈值/索引/日志/推断/事件提取/utt/examples/bridge 参数组，元数据驱动表单引擎 10 组 56 字段）；每字段默认值标注 + 恢复默认，`log_full_prompt` 开启弹窗隐私确认。

### 修复

- P0（真实消息导入测试，`docs/dev-1.4/测试报告.md`）：前端创建会话 `persona_uid` 恒为 NULL 导致保存会话错归默认人格（`create_session` 增加 persona_uid 参数 + `resolve_session` 回写绑定 + 抽屉告警）；utt/examples 在对话路径未生效（目标 persona 三层推断兜底）；保存时 L1 归属来源不统一（以 DB `sessions.persona_uid` 为真相源）；L3 空数组响应被误判解析失败 → 降级 mock 污染画像（空数组视为合法响应）；孤儿活跃会话遗留（空闲检测遍历全部活跃会话）；「深度处理导入的消息」LIMIT 200 只覆盖 4 个 session（移除限制）。
- 既有缺陷：`token_budget.rs` 多字节截断超预算（`find_last_sentence_boundary` 字节索引 → 字符索引）；RAG 截断 `fit_chars` clamp；预算分配块间分隔符计入。
- Qwen3-Embedding 0.6B 导入校验失败（`sliding_window: null` + `head_dim`）——改用 candle `qwen3::Config` + 内嵌无状态 Qwen3 前向。

### 破坏性变更

- `memory_l1.evidence_notes` 格式升级为结构化对象数组 `[{text, time?, who?, cause?}]`，存量行一次性迁移（无运行时兼容解析，迁移前备份原值）。其余变更均为增量（新增 `utt_blocks` 表、新增配置组）。

### 说明

- M7 探针实验不在 v1.4 执行（决策 D-V14-010）：探针改造为 CLI 自动化工具链（T1→v1.5、T2→v1.6、T3→v1.7），utt 切分/召回/说话人标注参数维持 v3.1 初值（待实证）。

---

## [1.3.0] - 2026-07-22

### 核心特性

#### TopicBatcher 主题聚类分批

替代了之前"按时间截取 N 条"的 L1→L2 事件提取批次组织方式。新的 TopicBatcher 通过关键词 Jaccard 图（α=0.5 与 L1 embedding 语义融合，无 embedding 时自动 α=1.0 降级）构建簇关系，经连通分量 BFS 后再以模块度 Q 二分递归拆分超大分量（> 25 条时 Q < 0.3 停止）。小于 3 条的碎片簇进入 Pending Buffer，同类积累到阈值后自动提升为正式簇，30 天未归并则降级合并；孤立节点做语义吸附归入最近簇。最终各簇按平均 salience 降序排列。相关代码位于 `ramaria-memory/src/event/batcher/`（mod.rs ~1100 行、graph.rs ~600 行、buffer.rs ~500 行），新增约 78 个单元测试覆盖图构建、模块度拆分、缓冲区管理、语义融合和端到端编排。

#### 关键词体系 Newtype 升级

在 `ramaria-core` 中新增 `KeywordToken` Newtype（自动 trim + 英文小写 + 非空校验 + 256 字符上限），配套 `KeywordSet` 去重有序集合、`KeywordStatus` 三态枚举（Canonical / Alias / Pending）和 `KeywordRef` 倒排引用枚举。归一化方面，`ramaria-memory` 新增 `BigramWithDictionaryNormalizer`（最大正向匹配，词典按长度降序 + 别名解析）和 `AliasManager`（正向/反向双缓存，文本相似度 + 高使用量别名反转）。Schema 层面新建 `keyword_refs` 倒排索引表，同时激活了 v1.2 预埋的 `keyword_pool.canonical_id` 和 `alias_status` 列。L1 Summarizer 的关键词输出和 BM25 分词器均已接入新体系。新增约 95 个单元测试。

#### L1 evidence_notes 双层摘要

`memory_l1` 表新增 `evidence_notes TEXT` 列，L1 Summarizer Prompt 同步增加对应的字符串数组输出字段，`L1SummaryResponse` 以 `Option<Vec<String>>` 接收并兼容缺失字段。后处理对 None、空数组、全短条目（< 5 字符）三种情况做降级处理，仅记 warn 日志，不阻塞 L1 生成。

#### CompositeIndex 三级上下文检索

新增 `ContextRetriever`，在事件提取前为每个 TopicCluster 检索历史上下文，按精确匹配 → 子串匹配 → 语义模糊三级递进编排（嵌入模型不可用时自动跳过语义层）。检索到的历史上下文以独立段落注入提取 Prompt，附"仅供背景参考，不得据此编造新事件"的约束和去重指令。为支撑这一功能，Retriever 新增了 `search_exact()`（内存 HashMap 精确命中）和 `search_substring()`（BM25 bigram 子串匹配）两个公开方法。

#### 事件提取 Prompt 改版：motives 激活与事件关系

L2 EventExtractor Prompt 输出格式从纯数组升级为 `{"events": [...], "relations": [...]}` 结构体。每事件新增 `motives` 字段（Fundamental Motives Framework 七类动机候选池），`memory_events.motives` 列从硬编码 None 改为 LLM 输出写入。事件关系 6 种类型（CausedBy / PartOf / RelatedTo / ContinuedBy / Contradicts / Timeline）正式激活写入，含索引边界校验、自引用过滤和非致命错误降级。解析层新增 `EventRelationOutput` / `EventExtractionResponse` / `ParsedExtractionResult` 三个类型，采用四步 JSON 解析确保鲁棒性。

#### Phase A 统计方法深化

校准权重链从简单 `salience × situation_multiplier` 升级为四因子乘积 `salience^cal × confidence_factor × situation_multiplier × source_support`。准入机制从单一的 confidence ≥ 0.6 硬截断改为 confirmed / tentative / discarded 三轨动态准入，tentative 事件跨批次复现时自动提升至 confirmed。收缩估计从单一全局先验升级为分层选择：Base/Primary 层使用跨领域全局先验，Accent 层使用领域/主题簇先验，Phase B 的 layer 标注反哺 Phase A 选择。新增 motives 维度统计（`MotiveStats` + `group_by_motive()`），统计文本注入 Phase B Prompt。旧版兼容路径通过 `use_calibrated_weights = false` 保留。相关代码主要位于 `ramaria-memory/src/inference/stats.rs`、`shrink.rs` 和 `orchestrator.rs`，新增约 78 个单元测试。

#### A8 因果链分析

新增 `ramaria-memory/src/inference/causal.rs`，实现了因果链特征提取流程：基于 CausedBy 关系构建有向图，从入度为 0 的源节点 DFS 寻最长因果路径，对事件类别序列分组检测循环模式（≥ 2 次出现），去重后保留前 5 个模式。特征文本通过 `format_causal_features_text()` 注入 Phase B Step 1 Prompt。`StorageBackend` 新增 `list_event_relations_by_persona` 方法支撑数据查询。新增约 19 个单元测试。

#### 跨版本簇匹配

Phase A 聚类后为每个簇生成语义标签（从核心样本 paraphrase 按中文标点切分短语 → 频次统计 → 前 3 个高频短语拼接），经 embedding 向量化后持久化到 `persona_cluster_snapshots`（新增 `semantic_label` + `semantic_label_embedding` 列）。跨版本匹配以 cosine similarity 比较新旧标签。新增约 12 个单元测试。

#### Phase B/C 适配新统计指标

Phase B Step 2 Prompt 增加了"贝叶斯收缩后"的标注说明，增强不同分类间统计值的可比性。Step 1 新增"话题 vs 性格"语义区分指令。Step 3 新增置信度差异化指导（n_eff ≥ 10 → 0.7-0.9，5-10 → 0.4-0.7，< 5 → 0.2-0.4）。Phase C 的漂移检测从三维度扩展为四维度（新增 `salience_drift` + `confidence_drift`），置信度更新适配了新校准权重链（每条证据贡献 = `calibrated_weight × |score| × decay`），事件到 trait 的分配改为按最长公共子串比例匹配而非全量广播。

#### 前端 L3 三层性格展示

新增 `trait-evidence.js`（~290 行）和 `trait-evidence.css`（~290 行），实现可展开的证据链组件。L3 Tab 按 base/primary/accent 三层分区渲染，卡片布局配合不同左边框色区分层级，顶部设数据状态指示器（可信/初步/数据不足 + n_total_eff）。后端新增 `get_personality_profile()`、`get_trait_evidence()` 和 `get_profile_status()` 三个 Tauri 命令，配套 6 个 View 结构体支撑 trait → event → L1 → evidence_notes 完整溯源链的传输。CSS 约 240 行新样式，含置信度色条（绿 ≥ 0.8 / 黄 0.6-0.8 / 橙 < 0.6）和暗色模式适配。

### 审查修复

#### 安全

导入路径增加了 `canonicalize` 解析和符号链接跨越白名单目录的拒绝校验，覆盖 `analyze_qq_chat`、`detect_qq_format`、`import_qq_chat` 三个入口。导出路径限制在用户文档目录（Documents / Downloads / Desktop）内。Prompt injection 防御方面，memory_context 以 `<memory_context>` XML 标签包裹，`sanitize_user_message()` 检测 10 种注入模式并追加防御性前缀。

#### 可用性

新增 LLM health_check 启动探测（轻量 GET，5s 超时，3 次重试间隔 2s），失败时非阻塞地置为 Degraded 状态。将嵌入推理从持锁执行改为 clone Arc 后锁外执行（< 1μs 持锁时间）。清理了 README 和 CHANGELOG 中残留的 Qdrant 引用，全部替换为 BruteForceIndex，并删除了 `qdrant_poc.rs`。`facts` 统计从多次循环查询改为单条 GROUP BY SQL。SSE 增加了单行 10KB 截断、无换行缓冲区插入和 120s 整体超时三个限制。

#### 性能

向量索引增加了基于查询向量量化哈希的 128 条目 LRU 缓存，索引变更时自动清空。检索器从 `Mutex` 改为 `RwLock`，允许多读并发。内部事件 channel 从无界改为有界（容量 64），`try_send` 满时丢弃并记 warn。消息加载改为分页，每页 20 条，200 条上限，6000 字符预算。

#### 代码拆分

`session_lifecycle.rs`（~940 行）拆分为 `session_lifecycle/` 目录 4 文件：`mod.rs`（编排入口 ~320 行）、`idle.rs`（~155 行）、`l1_generate.rs`（~320 行）、`l2_l3_scheduler.rs`（~380 行）。`app.rs` 提取出 `app_setup.rs`（run_setup / probe_health_with_retry / refresh_setup_state）和 `app_privacy.rs`（check_privacy / confirm_privacy），主文件降至 ~441 行。

### 测试修复

经过三轮集中的端到端测试与回归修复（覆盖 QQ 7083 条消息的导入全流程），共修复 20 项缺陷，主要集中在以下几个模块：

**L1 摘要生成**：系统人格不再显示全部导入 LLM 的 L1 记录；导入时为对话双方各生成一份独立的 L1 摘要；L1 空前缀和 persona 名称替换问题跨 6 个文件修复。

**L2 事件提取**：事件关键词限制为中文；态度推断在闲聊场景中也能正常触发；移除了 L1 降级查询 fallback 导致的噪声；事件角色关系通过 Prompt 增加角色区分规则和 `other_persona_name` 参数来解决交叉污染。

**L3 性格推断**：置信度从统一 50% 改为基于 n_eff 区间的差异化指导；trait 标签从话题名改为真正的性格描述（Prompt 增加语义区分指令 + mock_infer 话题词黑名单）；置信度完全相同的问题通过 Phase C 改用 `compute_event_trait_relevance`（最长公共子串比例）匹配事件到 trait 来解决。

**前端交互**：L1 滚动位置通过 `_savedState.l1ScrollTop` + `requestAnimationFrame` 恢复；证据面板首次展开空白的问题经两轮修复（增加 traitId 参数校验 + async 恢复按钮）解决；应用内人格的聊天口吻通过新增 `SHARED_CHAT_STYLE_RULES` 常量和 `load_persona_toml_fallback` 注入来适配社交平台的表达习惯。

### Schema 变更

新建 `keyword_refs` 倒排索引表（keyword_id FK→keyword_pool，doc_type，doc_id，persona_uid，weight），含双索引。`memory_l1` 新增 `evidence_notes TEXT` 列。`persona_cluster_snapshots` 新增 `semantic_label TEXT` 和 `semantic_label_embedding BLOB` 列。`keyword_pool.canonical_id` 和 `alias_status` 列从 v1.2 预埋状态正式激活 CRUD。

### 工程改善

全 workspace 测试总数达到 700+（v1.2 基线约 600），新增约 100 个测试，覆盖 M4 全链路集成、关键词倒排索引、TopicBatcher 端到端和 L2→L3 全流程。真实 LLM Smoke Test 在 LM Studio / DeepSeek / OpenAI 三后端各通过一次。新增 `ramaria-core/src/keyword.rs`（~650 行）、`ramaria-memory/src/keyword/` 目录（~1000 行）、`ramaria-memory/src/event/batcher/` 目录（~2200 行）、`ramaria-memory/src/event/context_retriever.rs`（~450 行）、`ramaria-memory/src/inference/causal.rs`（~650 行）。同步更新了 5 份开发文档和 README，将决策 SSOT 升级至 v8.0。

### 破坏性变更

关键词从裸 `String` 升级为 `KeywordToken` newtype，影响所有涉及关键词的公共接口，保留 `From<String>` / `Into<String>` 向后兼容转换。L1→L2 事件提取的批次组织从"按时间截取 N 条"完全重写为 TopicBatcher 语义聚类分批。Phase A 统计权重从简单 `salience × situation_multiplier` 改为四因子校准权重链 + 三轨准入，L3 推断的输入权重和准入门槛均有变化。`StorageBackend` trait 新增了 `list_event_relations_by_persona`、关键词倒排 CRUD、`list_messages_paginated`、`get_all_snapshots_with_embeddings`、`count_all_facts_for_persona`、`list_event_sources_by_event` 等多个方法。

### 已知限制

与 v1.2.0 相同：仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用；应用图标为占位文件；不支持 LLM 对话"重新生成"功能；ONNX 模型需用户手动下载或配置。

---

## [1.2.0] - 2026-07-07

### 核心特性

#### Pipeline + Stage 架构重构（🔴 P0）

- 将 `send_message` 10 步单体方法重构为 Pipeline + Stage 模式
- `PipelineStage` trait：统一接口，关联类型 `Input`/`Output`，`async execute()`
- `PipelineContext`：全 `Arc` 引用共享上下文，零拷贝传递（storage/llm/embedding/config/retriever/keychain/lifecycle）
- `PipelineData`：数据载体，承载 10 个 Stage 的中间结果
- `PipelineError`：区分 `Retryable`/`Fatal`，编排器在第一处 Fatal 错误时中止
- `SendMessagePipeline`：编排器，按顺序执行 Stage 序列
- 10 个独立 Stage，各自可注入 mock 依赖编写确定性单元测试
- 新增 ≥ 60 个单元测试 + 集成测试覆盖全流程正常路径和错误传播

#### Session-Persona 绑定（🔴 P0）

- `sessions` 表新增 `persona_uid TEXT` 列（增量 migration，`DEFAULT NULL` 兼容存量）
- `create_session` 签名新增 `persona_uid` 参数，创建时写入当前对话人格
- 用户消息 `persona_uid` 统一填入当前对话人格（不再为 NULL）
- Persona 切换由后端主导：优先从 `session.persona_uid` 读取，NULL 时回退前端传参
- 前端 `personaSessions` 降级为性能缓存
- 导入历史会话绑定正确人格（`create_historical` 新增 `persona_uid` 参数）

#### L3 管线贯通（🔴 P0）

- 新建 `ramaria-memory/src/inference/orchestrator.rs`：`run_phase_b_inference` + `run_phase_c_update`
- Phase B 三步 LLM 结构化推断完整接通（逐分类信号→跨分类一致性→合成三层画像）
- JSON 三步解析 + 五档钳制降级策略；LLM 全失败时回退 `mock_infer` 产出 `TraitSource::Statistical`
- Phase C 置信度更新 + Wasserstein 漂移检测 + 证据链记录
- Phase B/C 写入 `personality_traits` + `trait_evidence`；完成后标记事件已吸收
- `run_l3_inference` 全流程（Phase A→B→C）在 mock LLM 下跑通
- 新增 `InferenceConfig` 配置项（含 4 个子配置，合理默认值）
- 新增 ≥ 37 个测试（30 个纯函数 + 7 个端到端集成测试）

#### 前端记忆与对话联动（🟡 P1）

- **SessionDrawer 组件**：对话页左侧会话历史抽屉，点击 Header "📋 历史"按钮滑出
  - 180ms slide 动画、搜索过滤、活跃/已关闭/导入标签区分
  - 点击会话项加载消息，已关闭会话自动只读
  - 加载骨架屏 + 错误重试 + ESC/外部点击关闭
- **L1 记忆卡片跳转**：卡片底部"💬 查看对话 (N 条消息)"按钮
  - `Router.showView` 扩展 `options`（`sessionId`/`personaUid`/`fromView`）
  - ChatView 顶部"← 返回记忆"面包屑，记忆页恢复之前状态
- **L1 卡片 UI 重新设计**：
  - valence 情感色条（正面=粉渐变/负面=蓝渐变/中性=灰），顶部 3px
  - 属性行并排展示（时段 + 氛围 + 参与人数）
  - 关键词 chip 标签替代逗号分隔文本
  - 底部操作栏（时间 + 强度条 + "💬 查看对话"按钮）
  - 旧卡片降级兼容（无 `context_json` 隐藏参与人数，无 `session_id` 隐藏跳转按钮）
- **导入进度 UI 增强**：进度条高度 ≥ 10px、阶段指示器"第 N/M 个会话"、预估剩余时间、暗色主题适配

#### 后端记忆持久化修复（🔴 P0）

- 空闲超时自动关闭和 shutdown 关闭路径的 `persona_uid` 不再丢失
  - 修复前：硬编码 `None`，L1 摘要归属 NULL → `list_recent_l1_by_persona` 查询不到
  - 修复后：从 active session 的 DB 记录读取 `persona_uid` 传入
- L1 摘要生成后立即增量更新 Retriever 内存索引
  - 新增 `Retriever::index_l1_record(&MemoryL1)` 公开方法
  - L1 生成后立即可通过 Stage 5 RAG 检索命中，不需等待手动 rebuild
  - BM25 通道可即时命中（向量通道需 rebuild 路径生成）
- `App::new` 注入共享 `Arc<Mutex<Retriever>>` 到 `SessionLifecycle`
- 新增 9 个单元测试覆盖空闲/shutdown 路径 + Retriever 增量索引

### Schema 变更

- `sessions` 表新增 `persona_uid TEXT`（增量 migration，DEFAULT NULL）
- `memory_events` 表新增 `motives TEXT`（v1.3 激活，v1.2 仅预埋 schema，不修改业务逻辑）

### 工程改善

#### 测试

- 全 workspace 测试总数 ≥ 600（v1.1: 546，新增 ≥ 50 个）
- 新增 M1/M2/M3 集成测试文件（Mock 全依赖 Pipeline 流程 + L3 闭环验证）
- 新模块行覆盖率 ≥ 80%（`pipeline.rs`、`stages/`、`orchestrator.rs`）

#### 代码组织

- 新建 `ramaria-app/src/pipeline.rs`（~1320 行）+ `stages/` 目录（10 个 Stage 文件）
- 新建 `ramaria-memory/src/inference/orchestrator.rs`（~1400 行）
- `app_chat.rs` 逻辑拆分至各 Stage（`search_and_assemble_context`、`build_system_prompt_with_context` 等）
- `RunL3Inference` 中 `_llm` → `llm`，Phase B/C 调用链完整

#### 文档（v1.2）

- `chat-spec.md`：管线架构更新为 Pipeline + Stage 模式；Session-Persona 绑定；SessionDrawer；Retriever 增量索引
- `memory-spec.md`：L3 闭环 Phase A→B→C 全流程；orchestrator；L1 卡片跳转
- `arch-decisions-unified.md`：延后清单标注 L3 Phase B/C 在 v1.2 完成；`motives` 列已预埋
- README：版本号 v1.1.0 → v1.2.0；测试数量 ~600+

### Bug 修复

- 修复空闲超时/shutdown 关闭 session 时 `persona_uid` 丢失（L1 摘要归属 NULL）
- 修复 L1 生成后 Retriever 不更新的时序空隙（保存后立即检索命中）
- 修复导入历史会话 `persona_uid` 为 NULL（SessionDrawer 按 persona 筛选失效）
- 修复 SessionDrawer 竞态条件（`_isOpen` 过早设置导致 outside-click 处理器立即关闭抽屉）
- 修复对话界面空白（`chat.js` 多余 `*/` 导致 JS 语法错误）
- 修复 Embedding 查询失败（`llama_head_dim.rs` 未清除 KV cache）
- 修复保存对话后重进只显示旧会话（`personaSessions` 缓存未清除）

### 破坏性变更（开发者）

面向终端用户无破坏性变更。以下为内部 API 变更，不影响功能：

- `StorageBackend::create_session` 签名新增 `persona_uid: Option<&str>` 参数
- `create_historical` 签名新增 `persona_uid: &str` 参数
- `App.retriever` 类型从 `Mutex<Retriever>` 改为 `Arc<Mutex<Retriever>>`

### 已知限制

与 v1.1.0 相同的限制：
- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件
- 不支持 LLM 对话"重新生成"功能
- ONNX 模型需用户手动下载或配置

v1.2 新增：
- 存量 session（v1.1 及以前）`persona_uid` 为 NULL，在 SessionDrawer 中按 persona 筛选时归入默认人格。不影响正常对话，下次关闭 session 时自动填充。

---

## [1.1.0] - 2026-06-16

### 核心特性

#### Session 生命周期与记忆管线全自动触发

- 手动关闭：用户点击"保存对话"→ session 关闭 → L1 摘要 → 级联检查 L2/L3 触发。同一窗口继续对话，不清屏
- 空闲自动关闭：后台线程每 60s 轮询，空闲 > 10min 自动关闭 session 并触发记忆管线
- 只读约束：已关闭 session 禁止写入（DB 层拒绝 + 前端隐藏输入框），显示"此对话已关闭"提示
- shutdown hook：应用退出时自动关闭活跃 session，取消后台任务

#### 本地嵌入模型

- 集成 ONNX Runtime（`ort` v2.0-rc.12），运行 `bge-small-zh-v1.5`（384 维），feature gate `embedding-onnx`
- 模型下载管理：进度回调 + SHA-256 校验 + 断点续传
- BM25-only 降级模式：未配置嵌入模型时自动切到 `Degraded` 状态，RAG 仅用 BM25+图谱通道
- 对话页顶部进度条：下载/索引进度展示，5s 无事件自动隐藏
- RAG 检索适配 8 种通道组合（BM25/向量/图谱任意组合）

#### 情境强度加权 + Token Budgeting

- `memory_l1` / `memory_events` 新增 `situation_strength` 字段（1-5 级，默认 3）
- Phase A 统计推断加权：弱情境(1-2)×1.5、中性(3)×1.0、强情境(4-5)×0.5
- Token 预算分配：字符数估算(CJK≈len/2, 拉丁≈len/4) → System Prompt(1000) → RAG → History(新→旧)
- 句子边界优雅截断（`。！？\n`），不硬切

#### QQ 聊天记录导入器

- 新建 `ramaria-importer` crate（workspace 第 8 个 crate），compile-time feature gate
- 双格式支持：JSON（`qq-chat-exporter` v5.x）+ TXT（经典 PCQQ 导出）
- 多编码兼容：UTF-8 / UTF-8 BOM / UTF-16 LE / GBK
- 快速导入：仅写 `messages` 表 + `import_fingerprint` 去重
- 深度导入：历史 session → L0→L1→L2→L3 全管线
- 双画像自动创建：导出者和聊天对象各自独立 persona，UID 优先使用 QQ 号
- 角色前缀：`[烧酒] xxxx` / `[omkidaso] yyyy`，消除"用户 vs 助手"误导
- CLI: `ramaria import qq --file <PATH> [--deep] [--persona-self-name ...]`
- 桌面端：三步导入向导（文件选择→预览报告→确认导入）

#### 多角色管理 GUI

- Sidebar 新增 👥"人格"导航页，人格卡片网格展示
- 详情页在线编辑基本信息（名称/头像 URL/描述）
- 设为默认对话人格 / 重载性格按钮

#### 自动更新检查 + 诊断导出

- `check_update()`：GitHub Release API `/latest` + 语义版本号比较
- 设置页"诊断与更新"：版本号显示 + 检查更新按钮
- 诊断导出：日志(1000行) + config(脱敏) + schema_meta + OS 信息 → `.zip`
- CLI: `ramaria diagnostics --output <PATH>`

---

### 安全修复

- **CSP 收紧**：移除 `'unsafe-inline'`，行内脚本外部化到外部 JS 文件
- **errorText XSS**：`innerHTML` → `textContent`，防止 LLM 错误消息注入
- **路径穿越统一规范化**：CLI/Desktop 统一 `canonicalize()` + RootDir/Prefix 检查
- **窗口关闭超时恢复**：前端 N 秒未响应 → 自动回退 `hide()`，托盘始终可恢复
- **JobManager CancellationToken**：应用关闭时 `execute_with_retry` 优雅取消
- **job 状态标记失败终止**：不再静默继续执行
- **session list 真实查询 message_count**：SQL JOIN 替代硬编码
- **API Key 统一遮蔽**：前端显示 + 诊断导出统一 `[REDACTED]`

---

### 工程改善

#### 性能优化

- Retriever `l1_docs`/`l2_docs` 添加 LRU 淘汰（1500/1500 cap），防止内存无限增长
- storage 批量写入添加显式事务（`save_import_batch()`），减少 SQLite fsync 开销
- 前端 `_pendingDelta` 添加上限保护（超 10KB 强制刷新）
- BM25 `add()` 改为移动所有权 + `degrade` 使用 HashSet 去重 O(n)
- 模型下载 HTTP 客户端添加超时（30s connect + 3600s total）

#### 代码组织

- `app.rs` 大文件拆分：提取 `app_chat.rs`（644行）、`app_retriever.rs`（156行）、`app_state.rs`（209行）
- `app.rs` 从 1270→492 行（-61%）
- CLI `unsafe` 块补全 4 处 SAFETY 注释

#### 测试

- 总计 546 个测试函数（v1.0: ~530），覆盖全部 8 个 crate
- 新增集成测试 `tests/integration_tests.rs`（13 个跨 crate 测试）
- `ramaria-importer` 17 个单元测试 + 8 个双画像测试

#### CI

- 新增 `cargo llvm-cov` / `cargo deny` / `cargo audit` 三个非阻塞检查（仅报告）

#### 文档

- 桌面使用指南全文重写（新增人格管理/导入功能/诊断与更新/故障排除扩充）
- CLI 使用指南全文重写（新增 `ramaria import` / `ramaria diagnostics`）
- 隐私说明全文重写（新增导入数据/诊断脱敏/修正 CSP）
- 新建 `config/default.toml` 配置模板（9 节 130 行）
- README 数据库表清单修正（移除 5 个 ghost 表，补全 23 张表完整清单）

#### Schema 变更

- 3 个增量 migration（`situation_strength` / `event_situation` / `persona_description`），均可空、向后兼容
- 不创建新表，不修改既有列

---

### 已知限制

- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件，正式图标待设计师提供
- 不支持 LLM 对话"重新生成"功能
- ONNX 模型需用户手动下载或配置

---

## [1.0.1] - 2026-06-13

### 修复

#### 致命：全新安装后应用无法启动

- **插件配置反序列化错误**：`tauri.conf.json` 中 `plugins.dialog`、`plugins.notification`、`plugins.store` 使用了空对象 `{}`，Tauri v2 反序列化期望 `null`（unit 类型），导致应用在窗口创建前 panic 退出
- **影响**：所有不含开发依赖的干净 Windows 环境均受影响
- **修复**：三个插件配置值从 `{}` 改为 `null`
- **Schema URL**：`$schema` 从已失效的 `dev` 分支改为 `v2` 稳定分支

---

## [1.0.0] - 2026-06-12

### 核心特性

#### 分层记忆管线（L0→L1→L2→L3）

- L0 原始消息层：永久保留所有对话消息，标记发言人，按时间排序
- L1 单次摘要层：session 结束后 LLM 自动压缩，生成结构化摘要（summary + keywords + time_period + atmosphere + valence + salience）。关键词从 keyword_pool 优先选择，确保长期收敛
- L2 事件提取层：未吸收 L1 ≥ 5 条或超 7 天触发，提取离散事件（含 8 个推断属性）。LLM 不可用时自动回退到规则式降级生成
- L3 人格画像层：surface/behavioral/core 三层分级，share 分级（private/trusted/public）控制 RAG 注入范围。Phase A 统计推断 + Phase B LLM 推断 + Phase C 增量漂移检测
- 冷启动流程：首次加载人格时自动注入知识背景
- 全量重建管线：支持切换 LLM 后端后从 L0 重新提取全部 L1/L2/L3

#### 三通道混合 RAG 检索

- BM25 全文检索：自研 Rust 实现，关键词精确匹配
- 向量检索：BruteForceIndex 暴力余弦 + 本地 ONNX 嵌入（bge-small-zh-v1.5）
- 知识图谱检索：BFS 遍历实体关系图，召回关联历史记忆
- RRF 倒数排名融合：三通道结果加权合并
- Ebbinghaus 遗忘曲线衰减：记忆检索权重随时间衰减，salience 调制衰减速度
- Persona-Aware 过滤：按人格画像 share 分级过滤可注入的记忆

#### 事件→性格推断管线

- Phase A 统计推断：高置信度事件特征均值计算
- Phase B LLM 推断：示例精选→聚类→推断→校准四步流水线
- Phase C 增量更新：计算 drift 漂移度，确认迁移路径
- 置信度追踪：每项特征关联 evidence，可溯源至原始事件

#### LLM Provider 层

- LM Studio 适配器：无 API Key，完全本地推理
- DeepSeek 适配器：支持 deepseek-v4
- OpenAI 适配器：兼容所有 OpenAI API 格式服务
- SSE 流式传输：futures channel + tokio spawn 异步架构
- OS 凭据管理器：Windows Credential Manager 安全存储 API Key
- 统一重试策略：指数退避，鉴权错误不重试

#### Tauri 2 桌面应用

- 原生 Windows 窗口：960×720 默认尺寸，最小 640×480
- 粉蓝双色设计系统：CSS Tokens 变量体系，暗/亮双主题
- 系统托盘：最小化到托盘，托盘菜单快捷操作
- 通知推送：新消息通知、后台处理完成通知
- 关闭确认弹窗：托盘最小化 / 完全退出二选一
- 配置向导：5 步引导（后端选择→API 配置→测试连接→人格选择→完成）
- 记忆查看器：L1/L2/L3 分页浏览，支持删除 + 二次确认
- UI 组件库：Toast / Modal / Skeleton / Markdown 渲染器
- Markdown 白名单 sanitizer + CSP + XSS 防护

#### CLI 工具

- 9 个子命令：`setup` / `ask` / `chat` / `memory` / `session` / `config` / `persona` / `index` / `export`
- 交互式 REPL：色彩输出，历史记录
- 流式输出：`--no-stream` 关闭流式，`--json` 输出原始 JSON
- 数据导出：支持 JSON / Markdown 格式
- 隐私确认：`--yes` 跳过确认

---

### 新增功能

#### 存储层

- 23 张表 SQLite schema，一次性 migration
- 19 个 Repository，手动行映射避免 sqlx derive 侵入 core
- WAL 模式，多连接读写并发
- 数据目录：默认 `%APPDATA%\Ramaria\`，环境变量 `RAMARIA_DATA_DIR` 覆盖

#### 配置管理

- 统一 RamariaConfig 配置结构
- SQLite settings 表持久化，支持 CLI 读取和修改
- 多后端配置（LM Studio / DeepSeek / OpenAI）
- 人格 TOML 文件（`config/personas/*.toml`）
- 隐私确认按 `provider + base_url` 粒度管理

#### 安全

- API Key 存储在 Windows Credential Manager
- 日志不记录完整对话内容，敏感字段截断或哈希
- CSP 内容安全策略，前端零 eval()
- Markdown 白名单标签 + 移除事件处理器 + 禁止危险协议
- CLI 路径穿越防护
- 本地模式完全离线，不发起外部网络请求

#### 错误处理

- 8 种错误变体（Config/Storage/Llm/Privacy/Index/Validation/Io/Unsupported）
- 错误到用户友好提示的映射（ErrorHint）
- CLI 错误上下文，带原文引用的错误信息

---

### 工程改善

#### 项目架构

- 7 个 crate 分层设计（core → storage/llm → memory → app → cli/desktop）
- Workspace resolver="3"，edition="2024"，MSRV 1.85
- 零 I/O 依赖的 core 层作为类型边界
- async-trait 抽象，支持 mock 测试

#### 测试

- 600+ 个测试函数，覆盖全部 7 个 crate
- 集成测试目录 `tests/`，含 fixture 数据和 mock backend
- CI：build + test + clippy(`-D warnings`) + fmt(`--check`)
- Smoke test 清单：11 类 83 项

#### 文档

- README：项目总览、架构图、模块职责表、分层记忆详解、核心创新设计
- 桌面使用指南：安装→配置→对话→记忆→设置→故障排除
- CLI 使用指南：9 个子命令完整参考
- 隐私说明：数据流向、安全措施、权限说明
- 发行说明模板：标准化 13 节结构
- 4 个 GitHub Issue 模板（Bug / Feature / Help / Config）

---

### 已知限制

- 仅支持 Windows 平台（桌面应用），Linux/macOS 可通过 CLI 使用
- 应用图标为占位文件，正式图标待设计师提供
- 不支持 LLM 对话"重新生成"功能
- MCP Bridge / 导入器 / 自动更新 / 多角色 GUI 等功能已延后

---

## 版本历史概览

| 版本 | 日期 | 说明 |
|------|------|------|
| [v1.3.0](#130---2026-07-22) | 2026-07-22 | 算法深化：TopicBatcher + 关键词体系 + 校准权重链 + 三轨准入 + 分层收缩 + A8 + motives + L3 展示 + 审查修复 |
| [v1.2.0](#120---2026-07-07) | 2026-07-07 | 深度打磨：Pipeline 架构重构 + L3 管线贯通 + 前端联动 + 后端修复 |
| [v1.1.0](#110---2026-06-16) | 2026-06-16 | 首个增量版本：记忆管线接通 + 嵌入模型 + QQ 导入器 |
| [v1.0.1](#101---2026-06-13) | 2026-06-13 | 紧急修复：全新安装无法启动 |
| [v1.0.0](#100---2026-06-12) | 2026-06-12 | Rust 重写完成，首个正式发布版本 |
| v0.7.0 | 2026-05-09 | Python 版最终功能版本（维护模式） |

> Python v0.3.x–v0.7.x 的完整变更记录见项目根目录 [`CHANGELOG.md`](../../CHANGELOG.md)。
