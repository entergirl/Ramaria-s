# 变更日志

本文档记录 Ramaria Rust 版的所有显著变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

---

## [2.6.0] - 2026-10-10

### 版本定位

**「消息管理底层整合与升级」**：把消息底座一次做齐，为后续社交通道复用打地基——导入链路引入**入站规范模型**（消息 / 发送者 / 附件统一表示）；消息持久化新增**发送者身份**（消息表两列 + 会话成员表）；**群聊导入支持**（全发言者各建人格、消息按发送者归属、摘要按块内参与者分发、事件与画像按人独立沉淀）；**图片理解链路**（导入图片登记为附件表、按块"先理解后摘要"、读取时注入图片描述、能力声明 + 首次自动探测 + 隐私门禁）；导入后处理收敛为服务层**单一入口**。**本版为内部结构版本，不设发行版**（不单独构建、发布安装包；用户下载面维持 2.5.0）。

### 升级须知

- **两次纯增量 migration**（`20261007_v2.6_identity.sql` / `20261008_v2.6_attachments.sql`）：旧库直接可用——既有导入消息的发送者身份为空、附件行为空，行为与其他数据不受影响。
- **图片理解默认关闭**：`[vision]` 组的能力声明默认 `false`（未在「设置 → 高级设置 → 图片理解」声明支持的后端不会发送任何图片数据）；声明开启后按 `模型 + 服务地址` 自动探测一次，探测通过才执行理解。
- **导入行为变化（无破坏性）**：新导入的图片消息库内文本为 `[图片#{md5 前 8}]`（此前为 `[图片]`），读取时按图片描述呈现；既有数据不变。

### 新增功能

- **群聊导入**：选择群聊导出（`chatInfo.type=group`）导入时，按发言者建立各人物人格（跨会话去重），消息归属到对应人格；会话摘要按块内参与者分发，事件与画像按人独立沉淀；导入报告含成员分布。
- **图片理解（导入与阅读）**：图片消息在导入时登记为附件并按块批量理解（同一张图只理解一次），摘要生成前完成"先理解后摘要"；对话历史、会话浏览与原文片段按图片描述呈现（如 `[图片: 一张晚霞照片]`）；读取失败或无描述时保留占位符，不阻塞任何流程。
- **设置页「图片理解」**（高级设置）：能力声明开关与单批处理上限；声明关闭即完全不发送图片数据。
- **发送者呈现**：导入消息在历史浏览中显示说话人名称（群聊可区分多说话人）。

### 工程与加固（内部结构，不改变用户可见行为）

- **入站规范模型**：导入解析产物统一投影为 `InboundMessage`（发送者 / 附件引用 / 回复关系 / 平台消息 ID），导入器与后续社交通道共用。
- **消息持久化扩展**：`messages` 增 `sender_ref` / `sender_name` 两列（本地 / 外部接入消息为空）；新增 `session_members`（会话成员）与 `message_attachments`（附件与理解结果）；表 27 → 29 张。
- **导入后处理单一入口**：`run_import_post`（图片理解 → L1 批量生成 → 深度级联自动携带总调用数）收敛命令行与桌面双路径，消除宿主漏调与进度事件缺值风险。
- **L1 生成扩展**：支持"一次生成 × 块内参与者复制分发"（各行独立吸收，L2/L3 触发零改动）；契约面：消息对象 6 → 8 字段，命令面 60 / 前端封装 58 / 子命令 19 / MCP 工具 6 / 事件面 6 零变化。
- **配置面**：新增 `[vision]` 组 2 键（模板逐键测试与前端默认值双向锁定同步）。

### 测试

分 crate 全绿：`ramaria-core` 180（另 1 doctest）、`ramaria-storage` 212、`ramaria-memory` 1335、`ramaria-llm` 89（含 doctest 1）、`ramaria-service` 636（`--features importer` 663）、`ramaria-mcp` 17、`ramaria-cli` 314、`ramaria-desktop` 68、`ramaria-importer` 82，全部 0 failed；前端 `node --test` 146 全绿；`clippy -D warnings` 与 `fmt --check` 零问题；结构自检 exit 0（阻断 0 / 例外 1）；parity golden 18/18 零 diff。全量 `cargo test --workspace` 回归由项目负责人执行。

---

## [2.5.0] - 2026-10-06

### 版本定位

**「主动对话完善」**：修正主动对话在"从未对话过的人格"（尤以系统用户人格为最）上误触发的问题，并新增**人格级主动开关**（默认「自动」：与某人格对话过一次后自动开启；可在设置页「主动消息名单」逐个管理）；补齐多人格打扰总量控制（全局日上限）与消息未读标记（会话列表未读计数、打开即已读、托盘数字徽标）。**本版含用户可见变更，设发行版 2.5.0**；无破坏性变更，旧库直接可用（一次纯增量 migration）。

### 升级须知

- **无需重建库**：仅一次增量 migration（会话表增"最后已读时间"列，存量会话按历史消息时间回填，升级后不会凭空出现未读）。
- **主动对话默认收紧（注意）**：只有"对话过一次"的人格才会自动主动开口；从未对话过的人格不再打扰。若升级前收到过某人格的主动消息但从未与其对话，升级后该人格默认静默——需要时在「设置 → 主动对话 → 主动消息名单」中将其手动设为「开启」。
- **系统用户人格不参与主动对话**：名单中该行显示但开关禁用（说明"用户人格不参与主动对话"）。
- **安装版通知更可靠**：安装包为通知身份注册应用标识（AUMID），安装版通知显示与点击定位不再依赖开发机手工注册。

### 新增功能

- **人格级主动开关 + 设置页「主动消息名单」**：逐人格三态控件（自动 / 开启 / 关闭）与状态文案（自动开启（已对话过）/ 未解锁（对话一次后自动开启）/ 已开启（手动）/ 已关闭）；行内切换即时保存；未对话人格可手动开启（提示"主动消息可能缺少话题素材"）。
- **全局日上限**：全部人格合计每日主动投递上限（默认 `0` = 不限，即不收紧既有行为），位于「高级设置 → 主动对话」组。
- **未读标记**：会话抽屉显示未读计数（超过 99 显示 `99+`）；打开会话即标记已读（抽屉点击 / 记忆跳转 / 启动恢复 / 通知定位 / 回复完成五处入口）；托盘图标显示未读数字徽标，悬停提示未读数。
- **命令行只读探针 `ramaria probe baseline`**：查看主动对话的投递 / 回应 / 判据计数与配置口径快照（默认统计近 24 小时，`--window-hours 0` 表示不限窗口；只读，不回写）。

### 缺陷修复

- **主动消息误触发**：调度此前对全部活跃人格一视同仁，且"无历史视为足够空闲"反向放行，导致从未对话过的（含系统用户人格）更容易被选中。现加入人格主动开关闸门与 `user` 人格硬排除，此类人格零触达（不进入选题、不调判据、不落库）。
- **托盘徽标不可辨**：未读徽标底色由与品牌图标同色系的红色改为深色底 + 白字（亮度差 ≥ 0.25，16 px 托盘尺寸下边界可辨），并新增对比度回归守卫用例。
- **主动消息名单错误态**：读取失败时提示错误且不回显旧值。

### 安全与加固

- **安装器注册通知身份**：新增 NSIS 安装钩子，安装时写入 `HKCU\Software\Classes\AppUserModelId\com.ramaria.app`（`DisplayName`），卸载时清理；与安装器写入快捷方式的标识互为兜底。

### 工程与加固（内部结构，不改变用户可见行为）

- 桌面命令面 57 → 60（+2 名单读 / 写、+1 标记已读）；前端 `api.js` 封装 55 → 58；会话对象字段 7 → 8（未读计数）；消息对象字段 6、桌面事件面 6、MCP 工具面 6 零变化。
- 主动开关判定在服务层单点实现、**惰性判定**（读命令实时计算，后台零写入）；"对话一次"口径 = 该人格存在本地用户消息（排除导入历史与主动消息）。
- 全局日上限只需 `[proactive] daily_total_limit` 一键与 `proactive.state.global` 运行时状态键，不新建表；`config/default.toml` 无需重建。
- 配置面：`config/default.toml` 的 `[proactive]` 组 21 → 22 键（新增全局日上限；模板逐键测试与前端默认值双向锁定同步）。

### 测试

分 crate 全绿：`ramaria-core` 163（另 1 doctest）、`ramaria-storage` 193、`ramaria-memory` 1310、`ramaria-llm` 86、`ramaria-service` 620（`--features importer` 633）、`ramaria-mcp` 17、`ramaria-cli` 314、`ramaria-desktop` 68、`ramaria-importer` 53，全部 0 failed；前端 `node --test` 120 全绿；`clippy -D warnings` 与 `fmt --check` 零问题；结构自检 exit 0（阻断 0 / 例外 1）；parity golden 18/18 零 diff。全量 `cargo test --workspace` 回归与安装包构建由项目负责人执行。

---

## [2.4.0] - 2026-10-04

### 版本定位

**「主动对话」**：Ramaria 第一次主动发起对话——不依赖用户输入，由选题器挑选话题、经调度与打扰控制投放为桌面通知与应用内消息；主动消息是人格表达的一部分，正常参与全部记忆回流。同时完成一批收尾项（检索图谱通道由事件关系重建恢复非空贡献、待确认别名自动入池、WebView2 调试端口加固等）。**本版含用户可见变更，设发行版 2.4.0**；无破坏性变更，旧库直接可用（一次纯增量 migration）。

### 升级须知

- **无需重建库**：仅一次增量 migration（`messages` 增主动消息标记列，旧行自动回填 0）。
- **主动对话默认开启**（桌面）：首次启用有 3 天宽限期，之后按保守基线工作（距上次对话空闲 ≥4 小时、每人格每日最多 3 条、22:00-08:00 免打扰、两次投递间隔 ≥8 小时）；可在「设置 → 主动对话」一键关闭。
- **通知点击依赖系统应用标识**：Windows 通知点击回调需要应用标识（AUMID）已注册；开发环境用 PowerShell 标识兜底，安装包随安装器注册 `com.ramaria.app`。若通知不显示或点击无响应，见「已知问题」。
- **云端后端**：使用 DeepSeek / OpenAI 时，未完成隐私确认前主动对话静默跳过（不弹窗、不投递）；本地 LM Studio 不受限。

### 新增功能

- **主动对话**：
  - 调度与打扰控制：检查节拍、距上次对话最小空闲、每日上限、免打扰时段、投递冷却、连续未回应退避、首次启用宽限期；全部可在设置面板调整或一键关闭。
  - 选题器四源：高显著事件 / 未了结事件（关心跟进）/ 时间节点（事件后第 N 天）/ 行为规则情境，外加轻触达兜底；同一事件或规则有独立冷却窗口，不重复打扰。
  - AI 判据：由一次轻量调用决定「是否开口、聊哪个候选、用什么角度与语气」（输入仅含摘要，不含原文）；可关闭，回退算法打分决策。
  - 活跃时段：按历史作息软加权，低活跃时段不主动开口（样本不足时不启用）。
  - 呈现：桌面系统通知 + 应用内消息（气泡带「主动」标识）；点击通知回到对应会话。
- **设置面板「主动对话」**：基础面板四键（开关 / 每日上限 / 最小空闲 / 免打扰）+ 高级参数组（判据、活跃时段、效价与阈值、冷却等）。
- **检索图谱通道恢复**：由事件关系重建图谱通道，摘要路四通道（向量 / BM25 / 图谱 / 关键词镜像）全部实际参与融合（可用 `enable_graph` 关闭）。
- **待确认别名自动入池**：封存摘要写入关键词时，未命中但高度相似的新词自动登记为「待确认别名」，供两端列表整理（此前该列表只能手工 / 外部写入）。

### 缺陷修复

- **别名确认幂等统一**：已是合并状态的别名再次确认时统一返回成功（并标注「已是合并状态」）；命令行 `--json` 输出统一为四字段，桌面端同步返回该状态。
- **错误提示补充类别**：新增「嵌入模型错误」「数据序列化错误」两类中文标题与建议。
- **MCP 配置解析失败提示**：配置文件解析失败时，工具错误与启动自检带上原因与修复提示（无问题时文案不变）。
- **会话归属不一致提示**：显式会话与请求人格不符时记录警告（只记标识，不含消息内容），行为不变。

### 移除

- 移除已停用的 ONNX Runtime 嵌入后端（feature 声明、实现目录与 `ort` / `ndarray` 依赖同步删除），嵌入统一走原生 safetensors。

### 安全与加固

- **WebView2 远程调试端口清理**：release 启动时清除 `--remote-debugging-port` 注入（开发构建不受影响）。

### 工程与加固（内部结构，不改变用户可见行为）

- 前端死监听清理（三处无后端 emit 的监听与回调）；主动消息事件面 5 → 6（`proactive-message`）；消息对象字段 5 → 6（`is_proactive`）。
- 桌面命令面保持 57 项零变化（主动对话设置复用既有配置命令面）；MCP 工具面 6 项零变化（MCP 不装配主动调度）。
- 知识注入开关 `auto_fact_detect` 维持默认关闭（与主动对话知识注入场景同批评估结论）。

### 测试

分 crate 全绿：`ramaria-core` 163（另 1 doctest）、`ramaria-storage` 183、`ramaria-memory` 1310、`ramaria-llm` 86、`ramaria-service` 585（`--features importer` 598）、`ramaria-mcp` 17、`ramaria-cli` 309、`ramaria-desktop` 61、`ramaria-importer` 53，全部 0 failed；前端 `node --test` 104 全绿；`clippy -D warnings` 与 `fmt --check` 零问题；结构自检 exit 0（阻断 0 / 例外 1）。全量 `cargo test --workspace` 回归与安装包构建由项目负责人执行。

---

## [2.3.0] - 2026-10-02

### 版本定位

**「结构收尾与粒度治理」**：完成 2.2 未触及的"粒度债务"治理——全仓 647 个 Rust 源文件按粒度红线（生产 ≤700 行 / 测试 ≤1200 行 / 模块入口 ≤500 行）全量目录化，测试组织、导航层与文档按"按需加载"原则重构，结构自检进入 CI。同时完成 2.2 移交的全部收尾项（零调用命令清理、导出格式统一、索引首次构建链路修正等）。**本版含用户可见变更**（下方单列），**设发行版 2.3.0**；无破坏性变更，旧库直接可用。

### 升级须知

- **无需重建库**：无 schema 结构变更；新增一次增量 migration 仅在全新空库首次启动时将索引标记为"未构建"，既有库零改动（下次启动如遇索引缺失会自动重建，幂等无害）。
- **导出格式统一（注意）**：桌面端导出的 JSON 结构改为与命令行一致（带版本信封与格式化时间）——依赖旧裸数组格式的脚本需按新结构解析；命令行 `export --json` 结构不变。
- **用户下载面更新**：本版为发行版，提供安装包（2.1.0 之后的首次）。

### 变更（用户可见）

- **导出 JSON 结构统一**：桌面与命令行的导出 JSON 同构（版本信封 + 格式化时间 + 统一会话 / 消息字段）；桌面导出 Markdown 规则与命令行对齐。渲染收敛为服务层单点，两入口只做读写。
- **新增 `--redact` 脱敏开关**（命令行 `export` / `diagnostics`）：导出时关闭消息正文与摘要内容；诊断导出始终脱敏（该开关幂等）。
- **L2 事件浏览增强**：命令行 `memory l2` 改用服务层视图（分页下沉服务层）；桌面 L2 事件透传 `start` / `end` 字段。
- **新库首次启动链路修正**：全新安装首次启动执行"真实构建索引 → 就绪"，不再出现"索引标记与真实状态不一致"；索引自愈失败原因（脱敏）写入诊断包，便于排查。

### 缺陷修复

- **导入完成后上报的人格名**改为库内实际注册名称（此前可能与实际不符）；前端导入完成视图新增人格行。
- **错误提示统一**：业务校验与隐私类原文直出；技术类输出"场景 + 中文类别标题 + 原因"（不再出现英文类别串）；桌面 / 命令行 / MCP 三个入口统一映射，信封与退出码不变。
- **"待确认别名"写入侧接线**：聚类建议可自动落库为待确认项，两端列表入口触发一次建议生成（后续版本继续完善产出侧）。

### 移除

- 删除 4 个零调用桌面命令（`get_l3_traits` / `get_rule` / `delete_session` / `get_setup_status`）及前端封装（注册表 61 → 57，`api.js` 59 → 55）；"触发记忆管线"按钮接入既有管线命令。

### 工程与加固（内部结构，不改变用户可见行为）

- **文件粒度治理**：全仓 647 个 `.rs` 文件目录化拆分（纯搬迁，零 API / 行为变更；断言与用例名零改写、parity golden 零 diff）；结构自检阻断 89 → 0、例外登记 1（`StoreCrud` 单一 trait 契约）；红线与例外机制写入开发规范并进 CI（`structure-check` job）。
- **测试组织**：`xxx/tests.rs` 子文件模式定为全仓模板（保留私有访问、按需加载）；内联测试 ≥300 行外移；单测试文件 >1200 行按域再拆。
- **导航层**：`AGENTS.md` 改动导航四槽化（起点 / 连带改动 / 必跑测试 / 测试位置，7 个高频场景）；9 份 crate README 增文件地图。
- **文档结构化**：16 份现行开发文档增导读与分节索引；文档规范新增"导读与索引"一节（`STYLE-GUIDE` v1.2）。
- **CI 精简**：移除从未通过、无更新计划的 License Check 与 Security Audit 两个 job（`deny.toml` 保留待恢复）；覆盖率 job 保留。
- **编译时间对照**：收口对照开工基线——冷 check 191 s（基线 216 s）、9 crate 测试运行合计 72.3 s（基线 77.0 s）；数据留档 `docs/dev-2.3/test/compile-time-baseline.md`。

### 测试

分 crate 全绿：`ramaria-core` 155、`ramaria-storage` 171、`ramaria-memory` 1295、`ramaria-llm` 83、`ramaria-service` 495（`--features importer` 508）、`ramaria-mcp` 16、`ramaria-cli` 309、`ramaria-desktop` 48、`ramaria-importer` 53，全部 0 failed；前端 `node --test` 93 全绿；`clippy -D warnings` 与 `fmt --check` 零问题；结构自检 exit 0（阻断 0 / 例外 1）。全量 `cargo test --workspace` 回归与安装包构建由项目负责人执行。

---

## [2.2.0] - 2026-09-30

### 版本定位

**「内部结构收敛」**：把应用编排层（`ramaria-app`）的职责全部并入服务层（`ramaria-service`），使其成为唯一能力层；桌面 / CLI / MCP 三个入口薄壳化，workspace 由 10 个 crate 收敛为 9 个。**本版本无用户可见新特性**：对外契约与 2.1 逐项一致（61 项 Tauri 注册 = 60 业务 command + 1 托盘动作、19 个 CLI 子命令、6 个 MCP 工具、前端 `api.js` 59 条封装），唯一新增命令面为补齐 `save_file_dialog`；用户可见行为零变更，例外为下方单列的既有缺陷修复（逐条登记并附用例）。**本版为内部结构版本，不设发行版**（不单独构建、发布安装包；用户下载面维持 2.1.0）。

### 升级须知

- **无需重建库**：无 schema / migration 变更、无配置键新增（历史窗口与 L2/L3 定时阈值为读取既有配置键的口径修正），旧库直接可用；除下方登记的缺陷修复外，行为与 2.1 一致。

### 缺陷修复

- **补齐 `save_file_dialog`（D-V22-022，本版唯一新增命令面）**：设置页「导出诊断信息」此前因缺失保存对话框命令而点击无响应；现可正常弹出系统保存对话框选择保存位置，前端与调用封装零改动。
- **索引版本判定修正 + 缺索引自愈（D-V22-019 / D-V22-050 / D-V22-051）**：索引版本缺键不再被误判为已构建，按未构建处理；重建完成后写回版本；桌面启动 / 向导完成与 CLI 对话前各执行一次自愈重建（失败仅告警降级）。修复后缺索引的库可正常推进到就绪状态。
- **首次配置状态判定修正（D-V22-020）**：配置齐备 + 索引已建 + 嵌入可用即进入就绪状态，不再"必然停在索引中 / 降级"。
- **L2/L3 定时阈值配置化（D-V22-021 / D-V22-025）**：定时触发路径由硬编码改读 `[thresholds].l2_trigger_days` / `l3_trigger_days`（默认 7 / 30 天不变；`0` = 不按时间触发），与计数触发路径口径统一。
- **诊断导出失败占位脱敏（D-V22-038）**：日志 / 配置读取失败时写入诊断包的占位文本只保留文件名，不再包含完整路径。
- **MCP 历史窗口同步放宽（D-V22-033）**：MCP 对话历史窗口与桌面统一（默认 200 条 / 6000 字符预算），属口径统一，工具契约不变。
- **桌面遗留行为差异批次（D-V22-048 / D-V22-054）**：错误提示文案归并、记忆查询 `limit` 负值收敛、`persona reload` 失败行附加原因、导入前格式探测前置与 `.json` 扩展名严格化等（逐项登记见本版决策记录）。

### 工程与加固

- **应用编排层并入服务层并移除**：对话管线、会话生命周期与调度、索引重建、配置双写、状态机与全部用例编排并入 `ramaria-service`（唯一能力层）；`ramaria-app` 整 crate 移除（workspace 10 → 9 crate），代码与现行文档对应用编排层零引用。
- **入口薄壳化**：桌面 / CLI / MCP 的全部业务路径经服务层用例，入口仅保留宿主专属能力（Tauri 事件桥、托盘、通知、输出格式化、评估探针）。
- **单一实现**：封存、对话、召回包装、索引重建、空闲与 L2/L3 调度各只有一份实现；双轨编排与未接线管线清零。
- **测试迁移与基线化**：应用侧 15 个测试文件逐套件判定（149 例：迁移 119 / 删除 30，逐条登记依据）；4 条关键路径对照场景转为 golden 基线（累计 10 份）；新增入口装配 E2E 预检 6 例（离线 mock，约 1 秒）。
- **导航层**：新增 9 份 crate README（含「常见改动 → 文件 / 用例」映射）与本地 agent 工作导航 `AGENTS.md`（不随仓库分发），降低阅读与改动成本。

### 测试

分 crate 全绿：`ramaria-service` 342 + 18 平行对照 + 123 迁移套件 + 6 入口预检（`--features importer`：352 + 18 + 123 + 6）、`ramaria-mcp` 15、`ramaria-desktop` 43、`ramaria-cli` 286、`ramaria-memory` 1240 + 35、`ramaria-core` 154、`ramaria-storage` 166、`ramaria-llm` 63 + 20（5 忽略）、前端 `node --test` 93 全绿；`clippy -D warnings` 与 `fmt --check` 零问题。全量 `cargo test --workspace` 回归与三入口手工 E2E（真客户端挂载 / 回流可见 / 缺键库自愈 / 开关门禁）由项目负责人执行，记录见 `docs/dev-2.2/test/e2e.md`。

---

## [2.1.0] - 2026-09-25

### 版本定位

**「记忆服务化」**（3.0 路线第一块）：把记忆与人格能力从应用内部抽为与传输无关的服务层，并以 MCP（stdio）服务端对外提供六个工具——外部对话前端挂上即用本系统的记忆；回流内容在桌面可见并参与记忆加工。**本版本无破坏性变更**：migration 只增不删、`[mcp]` 默认关闭、桌面与 CLI 既有行为零改动，L0→L3 算法与检索融合规则未动。

### 升级须知

- **无需重建库**：仅新增一次增量 migration（`sessions` 增通道列与联合索引），旧库直接可用；未开启 MCP 时桌面与 CLI 行为与 2.0 完全一致。
- **需自备 MCP 客户端**：MCP 接入面向支持挂载本地 stdio 服务的客户端（CodeBuddy / Trae / DeepSeek Harness 等），默认关闭，需在桌面「设置 → MCP 接入」中主动开启。

### 核心特性

#### 记忆服务化与 MCP 接入

- **服务层 `ramaria-service`**：与传输无关的能力出口——召回装配（与桌面共用同一份实现，保证同源）、会话解析与封存、外部写入与去重、人格读取、空闲检查；禁止依赖任何入口层（编译期约束）。
- **MCP 服务端 `ramaria-mcp`**：stdio 协议壳（锁定 rmcp 3.4），六个工具：`memory_recall`（取记忆上下文，含概览模式）、`chat_send`（由本系统人格生成回复）、`chat_ingest`（外部对话写回，指纹去重、可触发封存）、`persona_list` / `persona_get` / `chat_history`。工具描述含使用时机，执行错误以结果内 `isError` 返回（模型可见并自行决策）；stdout 仅协议消息、日志走 stderr。
- **会话通道模型**：外部对话标识三级规则（显式 `conversation_id` > 客户端身份名 > 单流退化）；桌面会话列表标注「来源: MCP」。
- **封存与补扫**：惰性封存体检 + 宿主空闲循环 + 条件更新抢占（多进程下每会话恰好一份 L1）；L1 生成失败的补偿任务独立类型并逐条原子抢占，桌面与 MCP 共用同一份编排实现；MCP-only 运行下补扫同样生效。
- **跨进程索引刷新**：按索引代次与语料统计戳比对，桌面写入后 MCP 召回即时可见；嵌入模型懒加载，不可用时降级 BM25 + 关键词镜像。

#### 配置与界面

- **`[mcp]` 配置组（七键）**：`enabled` / `allow_ingest` / `allow_seal` / `allowed_personas` / `allow_raw_text` / `max_items` / `max_chars`；默认保守（总开关关闭、原文不出端），随默认模板与逐键测试锁定。
- **桌面「MCP 接入」面板**：总开关与运行状态、数据库路径展示与复制、客户端配置片段一键复制（通用 `mcpServers` JSON + DeepSeek Harness 插件 patch）、人格白名单、原文开关与写入开关、隐私提示（被检索内容会随对话发送给客户端所用模型）。
- **CLI 新增 `ramaria mcp serve`**：以 stdio 启动 MCP 服务端（沿用全局 `--db`）。

### 工程与加固

- **写冲突加固**：连接池 `busy_timeout`（10 秒）+ `SQLITE_BUSY` / `SQLITE_LOCKED` 有限重试（上限 3 次、线性退避，仅覆盖单条写）。
- **竞态修复**：L1 生成在途任务的 `pending` 窗口与补偿补扫混用会导致同一会话重复摘要——补偿登记改用独立任务类型 + 逐条原子抢占双保险修复。
- **封存门禁一致化**：`allow_seal` 下沉服务层统一门禁（封存、空闲检查、惰性体检同一口径），关闭时只写不封存并在回执说明。

### 测试

分 crate 全绿（core / storage / memory / llm / service / mcp / app / cli / desktop / importer），`clippy -D warnings` 与 `fmt --check` 零问题，前端 `node --test` 全绿；全量 `cargo test --workspace` 与真客户端端到端验证（CodeBuddy 挂载、回流可见、开关与隐私、多客户端并发）由项目负责人执行并确认通过，记录见 `docs/dev-2.1/test/m6-client-e2e.md`。

---

## [2.0.0] - 2026-09-13

### 版本定位

**「完备与实证」**（2.0 路线终点）：阶段一 M0~M7 一次性补全机制（关键词地基、知识召回、注入治理、画像收口、前端完备），阶段二 M8 用一份单人对单人高情感 QQ 记录做全档位探针实验完成参数与层价值定稿，阶段三 M9 整合发布。**本版本含一次授权破坏性变更**（schema 单基线 + 旧库重建），其余增强均走独立开关。

### ⚠️ 破坏性变更（升级必读）

- **migration 合并为单个 2.0 基线**（`ramaria-storage/migrations/20260905_v2.0_schema.sql`，**27 张表**，只 CREATE 不 ALTER）：v1.6 单基线之后全部表/列变更一次性并入，空库初始化即最终 schema。**存量旧库的 `_sqlx_migrations` 记录与新基线 checksum 不匹配，无法自动迁移**——升级路径 = **备份 → 重建空库 → 重新导入 → 数量核对**（见 `docs/dev-2.0/upgrade-path-2.0.md`）。重新生成 L1/L2/L3 的 LLM 成本由三层精确缓存（`llm_response_cache`，key 含模型 + 模板版本）大幅抵消。
- **`[l1.progressive].enabled` 默认值转正为 `true`**（渐进式摘要 B3）：长会话（消息数 >100 或跨度 >24h）按 `tail_msg_count`（60 条）切段、全段生成 L1。**存量 config.toml 中显式写有 `enabled = false` 的用户不跟随新默认**（文件显式值优先），需手动改为 `true` 或删除该行后重启；首启模板 `config/default.toml` 已同步为 `true`。
- 关键词补完带来的 BM25 分词变化经 `bm25_index_version` 检测，重建后自动全量重建索引（重建期旧索引降级可用、完成后原子替换）。
- 破坏性变更仅限 schema/migration 与 B3 默认值；不删除既有能力，其余行为增强全部保留独立开关（关闭即回退上一版本行为）。

### 核心特性

#### 关键词体系补完（M3）

- **统一标准化器**：`CommaSeparated` / `Bigram` / `BigramWithDictionary` 三种口径替换 5 处重复解析（BM25、L1 关键词、统计分类、示例选取、降级关键词）。
- **倒排索引 + 语义扩展**：`KeywordIndex`（精确 + 子串 + TF-IDF×Salience×Recency 评分）与 `CompositeIndex`（精确→子串→语义扩展三级回退）；`FuzzyKeywordIndex` 词向量后台异步构建，未就绪或 embedding 不可用时静默降级为字面两层。
- **词池三态状态机 + 别名 CRUD**：规范词 / 别名 / 待确认（confirm / reject），storage 补别名查询与状态迁移；`ramaria keyword list/show/seed/alias list|confirm|reject` CLI。
- **关键词系统接线**：`KeywordService` 会话级镜像（词池 + 复合索引 + 语义层），重建时同源装载、L1 生成后增量更新；作为摘要路**关键词镜像通道**参与 RRF 融合（可关）。

#### 知识召回与混合检索（M4）

- **三路参数解耦真正生效**：utt / RAG 摘要 / 知识 fact 三路各自独立 top_k、条数与路由阈值，改一路不影响另两路；`[retrieval]` 新增 RAG 格式化参数与 `enable_vector` / `enable_keyword_channel` / `keyword_weight`。
- **四通道 RRF 融合**：向量 + BM25 + 图谱 + 关键词镜像统一融合，缺通道不惩罚（按通道身份计权）；行为情境路由查询侧经词典别名归一。
- **auto_fact_detect 增强层**（默认关闭）：主观隐含事实补全、L1 线索→断言、低置信候选互证提升三策略；开关关闭零报告回退。
- **降低断言级依赖**：RAG 摘要为主召回、事实卡片为兜底，跨层证据引用级去重，同一事实不重复注入。

#### 注入机制与画像收口（M5/M6）

- **注入协调预算**（`[injection_budget]`，默认关闭）：RAG 与四层注入纳入同一 token 池，超限按 `order` 从低优先通道整块丢弃；固定骨架不入池。
- **层间证据去重与冲突仲裁**（`[layer_dedup]`，默认关闭）：同一事实跨层只注入一次，冲突按"手工 > 有引用 > 无引用"优先级保留。
- **提示词精简**：注入样板 849→558 字符（−34%），段落标题与语义不变；`PROMPT_TEMPLATE_VERSION` 升版，旧 LLM 缓存自动失效。
- **画像机制收口**：Phase C 漂移检测真实生效（从快照恢复旧分布，快照写点后移消除"同轮自比"）；跨用户冷启动先验与分层收缩真实接线；A8 因果链补"因果边时延分布 + 情绪沿链走势"；重述级联失效校验（信息保留度低于阈值降级保留原文）；风格小样本原文样例兜底。

#### 前端完备（M7）

- **规则管理页**：行为规则列表/详情/启用禁用/手工编辑/证据链查看（复用既有后端能力，无事实删除路径）。
- **调试面板**（设置中启用）：关键词池分层与别名确认/驳回、风格统计只读（五维 + 样本量 + 自动规则文本）、探针评估结果只读（档位得分 / 注入闸门语义 / 辅助指标）。
- **设置页**补齐 2.0 新配置组（三路检索、知识、风格、注入预算、层间去重、渐进摘要、画像升级开关等），前端默认值与 `config/default.toml` 逐键锁定。
- **安全与显示加固**：markdown 链接协议白名单 + 归一化（拦截控制符/实体混淆）、分泡单遍扫描状态机（代码块/链接/URL 豁免）、`chat-done` 携带全文（切视图不丢回复）、摘要页纯 DOM 写入 + API Key 只显掩码。

#### 数据驱动的参数定稿与评估（M8）

- **高情感库全档位探针**：15 档 × 90 题 × `--repeat 2`、0 失败；新增 TOST 等效性检验、事实维三口径判据（`legacy` / `norm` 主口径 / `point`）、语气维长度中性 judge v2 与客观形态指标、探针有效性自检（文档数为 0 判该轮无效）。
- **结论（单 persona 高信号数据）**：RAG 摘要基座是唯一显著正向的记忆通道（B0→B1 d_z 1.41~1.65）；四层专属注入无正净增量（严格等效仅 `I_behavior` 可证）；单层不能替代 RAG；知识层漏报按可及性轨 `norm` **2.5%**（聊天轨 13.7% 保留对照）；**7 个数值参数全部无显著敏感性 → 一律维持 v1.7 定稿值**；预算与层间去重实测不咬 / 结构性不可观测 → **维持默认 `false`**。
- 正式评估报告见 `docs/dev-2.0/test/v2.0-evaluation-report.md`（含 D2 效度与 D3 局限声明）。

### 工程与隐私

- **MSRV 升至 Rust 1.88**，workspace 统一 lint 入口；锁中毒恢复、字符边界截断、原子写、消息查询分页等全仓统一。
- **隐私加固**：导入路径 QQ 号/昵称日志脱敏（`mask_id`）、诊断导出二次脱敏（敏感字段记字符数占位）、配置模板与隐私声明与实现对齐。
- **性能**：向量缓存键改"L2 归一化 + 保号量化"、L1/L2/utt 批量驱逐（110% 水位）、图谱实体 bigram 倒排、BM25 词典增强 + 索引代次。
- **CLI/桌面**：`blocks` / `style` / `keyword` / `status` / `rule relearn|clusters` 等子命令（共 18 个）；命令层错误恢复 exit code 3；`--json` 信封与 stdout 纯净；桌面通知插件注册、日志 filter 收敛与静态隐私审计用例。

### 测试

分 crate 全绿（core / storage / memory / llm / app / cli / desktop / importer），前端 `node --test` 全绿；全量 `cargo test --workspace`、真实 LLM Smoke 与 Tauri 打包由项目负责人验收。

---

## [1.7.0] - 2026-09-04

### 版本定位

**「风格与闭环」**（2.0 路线 1.7 站）：表达层风格自动学会（A3，五维统计 + 显著性检验 + 自动规则生成）、长对话渐进式摘要 + 跨会话脉络按话题加权注入（B3/B4）、弱反馈自我修正闭环（H2 S2/S3）、正式四要素消融评估（J）。**全程不引入破坏性变更**：增量 migration（只增不删）、独立配置开关（关闭回退 v1.6 行为）、无 schema 结构变更。

### 核心特性

#### 风格统计（表达层 A3，v3.1 §7.2）

"自动学会 ta 的说话风格"：

- **五维风格指标**（`ramaria-memory/src/style/stat.rs`）：口癖词（相对超频 Top-N，复用 keyword 词表）+ 句式句长（分布均值/P25/P75、断句符频率）+ 标点（每 100 字感叹/问号/省略号/括号/波浪号）+ 情感表达（复用 `behavior/sentiment.rs` + 感叹词 + valence 标准差）+ 话题词汇偏好（名词词频 Top-N）；输出结构化参数（统计值 + 样本量）。分词复用 BM25 bigram + 内置停用词表。
- **全局基线池 + 显著性检验**（`style/baseline.rs`）：全部 persona 归一化合并池（增量更新，settings 键 `style_baseline_pool_v1`，JSON 不含原文）；二项 z 检验（`|z|≥2` 且 频次≥5 且 n_p≥200；口癖词另加相对超频比>2）；增量更新（小样本全量重算、大样本滑动合并）；冷启动回退。
- **自动规则文本生成**（`style/rule_gen.rs`）：模板拼接优先（确定性可测、零 LLM 降级）+ LLM 离线翻译增强（`[style].auto_translate` 开关，LLM 失败静默回退模板）；数据不足（n_p<200）标注不生成。
- **SpeakingStyle 落库 + 注入**：规则文本写 `persona_facts(field=SpeakingStyle, source=event, status=active)`（走版本链）；`render_style_block` 注入表达层 prompt「# 说话风格（表达层）」段落 + `## 自动风格规则` 子段；手工 `speaking_style`/`E_rules` 优先覆盖；封存时增量统计钩子（复用 behavior_hook 模式，`[style].enabled=false` 不注册）；知识层检索注入排除 SpeakingStyle（只读引用无副作用）。关闭/数据不足 → 不注入（回退 v1.6 语义等价，回归断言锁定）。
- **`persona_style_stats` 增量表** + `[style]` 配置组（enabled/auto_translate/min_sample_count=200/top_n/relative_boost_ratio=2.0/min_frequency=5/z_critical=2.0）。

#### 渐进式摘要 + 脉络加权（B3/B4，v3.1 §6.4/§6.5）

"长对话不丢信息、跨会话脉络按话题接续"：

- **渐进式摘要**（`[l1.progressive]` 配置组，默认关闭）：触发（消息数>100 或跨度>24h，可配置）；按 `tail_msg_count` 切段、全段生成，每段独立成 L1（absorbed=0 实时入候选池 + 入 TopicBatcher 缓冲），尾段覆盖最新对话；**L2 仍封存触发**（并发安全锁定）。`L1Summarizer::summarize_progressive` 复用 `generate_chunk_l1`。
- **流式超时/channel 满修复**：`transport.rs` SSE 超时分级——首事件 60s + 整体 600s（原双重 120s）；client 级超时 120→600；有界 channel 满时 `send().await` 背压（不静默丢 delta，接收端 drop 安全退出）。修复后长回复不截断、无静默丢 delta。
- **脉络加权注入**（`[retrieval] narrative_weighted=true/narrative_top_k=3`）：`Retriever::search_narrative`（BM25 相关性 × `calc_retention` 含访问加成融合排序，无相关性命中按时间兜底）；替换 v1.6"无条件取最近几条"。关闭回退 v1.6。
- **`MemoryL1::touch` 接线**：检索命中更新 `last_accessed_at`，激活 `[decay] recent_boost_*`（`calc_retention` 生效）；`SearchResult`/`L1DocView` 新增 `last_accessed_at` 全链传播。
- **graph_retriever 评分公式对齐**：入边参与 boost（与出边对称 `weight×0.1`），模块文档公式更新为 `relation_boost = 1.0 + Σ(关系权重×0.1)`。
- **mark_absorbed 事务化**：事件版逐批 autocommit 统一为事务化（`pool.begin()` + 命名占位符 + `tx.commit()`，BATCH_SIZE=100），杜绝事件半吸收。

#### 弱反馈闭环（H2，v3.1 §9，S2/S3）

"编辑过的规则与事实持续影响后续对话"——自我修正闭环（`feedback_log` 复用，v1.5 建表）：

- **S2/S3 信号检测**（`ramaria-app/src/feedback.rs`）：助手回复→用户消息间隔 ≤ 60s + 纠正前缀（`不对/不是/应该说/其实`）→ S2；窗口内非纠正 → S3；间隔超窗口不计（沉默/中断≠负反馈）。`correction_prefix_match` 返回脱敏前缀词。
- **weight 修正**：`core/behavior.rs` `SignalType::weight()`（S1=1.0 / S2(Correction)=0.6 / S3(Continue)=0.2），`FeedbackLog::new` 改用（替代原非强信号统一 0.6）。
- **`feedback_log` S2/S3 写入 + 排除项**：`process_feedback_for_new_message` 检测 → `should_dedup_recent_feedback`（30s 去重）→ 写 feedback_log（detail 只存纠正前缀词/间隔/信号类型，**不含原文全文**）；中断/沉默由窗口保证不计；超时封存由"仅活跃 session 检测"结构性保证。
- **校准机制**：S2 纠正 → 候选复审（不自动覆盖规则）；`ReviewCandidate`/`detect_s3_review_trend`（滑动窗口 20 次内连续 ≥5 Continue 后 ≥4 NotContinue → 标记复审）。
- **`auto_apply_weak_feedback` 默认关闭**（`[feedback]` 配置组）：false 时仅写 feedback_log（审计），复审队列（settings 键 `feedback_review_queue_v1`）不落库 → 规则/画像零自动修改（回归红线 5）。

#### 探针工具链扩展与消融评估（M5a + J，D-V17-015）

扩展 probe 工具链以支撑 M5b 正式消融评估，所有扩展向后兼容（可选字段/新模式，M1 产物不受影响）：

- **消融档位 Profile**：`AblationProfile` 枚举（B0/B1/F0/F1~F4/S_behavior/S_knowledge/S_expression/S_narrative，映射技术报告 §16.3 口径）；`RamariaConfig` 新增仅内存 `InjectionGate`（`#[serde(skip)]`，8 层全开默认；管线数据层闸门 + 渲染层闸门 `PromptConfig`，全关时整块省略）→ B0 无记忆块 / F1 无行为块可真实达成；`ablation=None` 与 M1 完全一致（契约测试锁定）。
- **emotion 第三维**：`ProbeDimension::Emotion`；`probe build` 3 维（tone/fact/emotion，qpd×3）；emotion 候选 = 情绪化 user 消息（情感线索过滤）→ persona 原回复配对（golden）；`evaluate` 对 emotion 用确定性 rubric 0/0.5/1（情境极性×安慰/共情或喜悦标记计数），非事实召回。
- **`--repeat` 逐轮评分聚合**：`evaluate` 检测 `repeat.per_variant[].rounds` → 每轮逐题评分取轮均分 → 跨 N 轮 mean/std/95%CI（t 分布），写入 `dimension_scores`（可选字段）。
- **消融对比报告**：`probe report --ablation`（需 --evaluation）；自动识别 F0/B1 基线，按题目配对 Wilcoxon 符号秩检验 + Cohen's d + 95%CI + BH-FDR 校正；判定线 p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0；辅助指标（平均回复字符/耗时/空回复率）。
- **评估执行 J**（真实 CUDA + DeepSeek 全量，11 档 × 90 题 × repeat=3，0 failed）：D1 数据集（鸢九三维各 30 题 + golden）→ S 组前置单层验证 → B0/B1/F0 基线 → F1~F4 逐层消融 → 统计报告。结论：事实维记忆注入整体正向（B1>B0），完整体系 F0 未高于压缩基座 B1；F1~F4 逐层关闭均无显著（无关键层）；S_narrative 显著负向。数据与产物见 `main/test-data/probe-m5/`，报告 `docs/dev-1.7/test/probe-test-report-J-v17-20260903.md`。

#### 探针定稿与环境陷阱修复（M0/M1）

- **探针可复算性统计法**：`probe run --repeat N`（多次运行 + 均值/置信区间，`--json` 信封遵循 v1.5 §2.8/§2.9）；temperature=0.3。
- **utt 参数定稿写默认配置**（档位实验，D-V15-014 基线 + 统计法 `--repeat 3`）：θ_gap **10** 分钟（10/30/60 中显著最优）、条数上限 **80**（40/80/100 无显著差异取大）、retrieve_top_k **3**（1 显著劣化）。`config/default.toml` + `config.rs` 已更新并注释标依据。
- **D-P 聚类参数定稿**：θ_nb=**0.65** / β1=**0.85** / β2=**0.10**（β3=0.05），min_cluster_size=3。
- **`--no-rebuild-utt`**：非切分档位复用已建 utt 块（embedding 调用数不随档位倍增）。
- **环境陷阱/审查待办收口（D-V17-013/014）**：
  - **CLI 中文路径编码**：`import qq --file` String→PathBuf（main.rs + import_cmd.rs + 测试适配），调用侧兜底文档化（`ProcessStartInfo.ArgumentList`）；**不新增 py/cmd 文件**。
  - **exit code 4 修正**：`probe evaluate`/`report` `--results` 缺失时 exit code 由 1 修正为 4（业务校验失败语义）。
  - **24 测试相关三项**：`extract_first_json_object` 括号深度感知解析（修复截断 JSON，+5 用例）；`RebuildConfig.rebuild_bm25=false` 实际禁用（`set_bm25_enabled` 临时禁用）；`alias.rs` ID 变更语义对齐（or_insert→insert）。
  - `build_evidence` 单测补强（新旧事件 weight 对比断言）。
  - 长任务后台执行 / CUDA 构建（VsDevShell + `cargo build --features ramaria-llm/cuda`）/ temperature 与 θ_gap 数据特性：**PowerShell 原生方案文档化**于 `docs/dev-1.7/test/`，不新增 py/cmd 文件。

#### auto_fact_detect 决策（D-V17-011）

M1 复核（定稿档位漏报率 20%，各档 10%~60%，无稳定 <10%）→ **未达标**；较 v1.6 T2（70%）显著改善；依决策按超标类目实装增强层，**具体实装范围待负责人裁决登记**（v1.7 未新增 auto_fact_detect 代码改动）。

### 说明

- **正式评估结论**（J）：四要素消融在 d1 数据集（鸢九，单 persona）未发现关键层——记忆注入整体正向但完整体系未超越压缩基座、逐层关闭无显著。工程结论与 2.0 改进方向（语气维人工抽检/本地 judge 补测、D2/D3 外部效度、S 组档位语义修正、高信号语料）记入 `docs/dev-1.7/备忘.md §九`，不阻塞 v1.7 发布。
- 知识层漏报 33.9%（评估执行未达标）——列为 2.0 输入。
- 待项目负责人验收：全量 `cargo test --workspace`；真实 LLM Smoke Test（风格规则注入/长对话分段/脉络加权/touch/弱反馈复审/探针定稿值/评估报告）。

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
| [v1.7.0](#170---2026-09-04) | 2026-09-04 | 风格与闭环：风格统计 A3 + 渐进式摘要/脉络加权 B3/B4 + 反馈环 H2 + 四要素消融评估 J + 探针定稿 + 环境陷阱修复 |
| [v1.6.0](#160---2026-08-26) | 2026-08-26 | 知识深化：知识层事实抽取/分层/仲裁 + 画像升级 + 降级矩阵 + 探针自动评分 |
| [v1.5.0](#150---2026-08-15) | 2026-08-15 | 行为驱动：行为层规则学习 + 驱动环注入 + 三层缓存 + B2 + utt 合并方向 |
| [v1.4.0](#140---2026-08-08) | 2026-08-08 | 原文通道：utt 话语块 + examples 激活 + 桥接 + 驱动环骨架 + 设置双写 |
| [v1.3.0](#130---2026-07-22) | 2026-07-22 | 算法深化：TopicBatcher + 关键词体系 + 校准权重链 + 三轨准入 + 分层收缩 + A8 + motives + L3 展示 + 审查修复 |
| [v1.2.0](#120---2026-07-07) | 2026-07-07 | 深度打磨：Pipeline 架构重构 + L3 管线贯通 + 前端联动 + 后端修复 |
| [v1.1.0](#110---2026-06-16) | 2026-06-16 | 首个增量版本：记忆管线接通 + 嵌入模型 + QQ 导入器 |
| [v1.0.1](#101---2026-06-13) | 2026-06-13 | 紧急修复：全新安装无法启动 |
| [v1.0.0](#100---2026-06-12) | 2026-06-12 | Rust 重写完成，首个正式发布版本 |
| v0.7.0 | 2026-05-09 | Python 版最终功能版本（维护模式） |

> Python v0.3.x–v0.7.x 的完整变更记录见项目根目录 [`CHANGELOG.md`](../../CHANGELOG.md)。
