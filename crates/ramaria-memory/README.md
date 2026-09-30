# ramaria-memory

> 定位：记忆与检索算法层——L0→L3 管线、混合检索（四通道 + RRF）、衰减、行为/知识/风格、Prompt 装配、共用召回。
> 上游 SSOT：`../../../docs/dev/03-memory-pipeline/`（memory-spec / event-extraction-algorithm / keyword-design）、`../../../docs/reports/对话-事件-人格画像系统技术报告.md`（数值口径）；决策见 `../../../docs/dev/00-architecture/arch-decisions-unified.md`。

## 职责

- **分层记忆管线**：L1 摘要（含渐进式分段）、L2 事件提取与吸收、L3 画像推断、示例与 utt 话语块构建。
- **混合检索**：向量 / BM25 / 关键词镜像 / 图谱四通道 + RRF 融合；`retriever/` 维护内存索引与驱逐；`recall.rs` 提供入口/服务层共用的 `assemble_recall`。
- **衰减与排序**：Ebbinghaus 衰减（`decay`）、相似度（`similarity`）、Token 预算裁剪（`token_budget`）。
- **Prompt 装配**：分层系统提示（`prompt/` + `chat.rs::build_system_prompt`，含模板版本常量）、脉络素材、示例预选。
- **行为 / 知识 / 风格**：情境路由与合并（`behavior/`）、事实版本链与判定器（`fact/`）、风格统计（`style/`）。
- **重建与重试**：`rebuild`（索引重建）与 `job`（后台任务类型与管理）。

## 公共入口

| 模块 | 主要内容 |
|------|----------|
| `recall` | `assemble_recall` / `RecallInput` / `RecallGates` / `RecallChannels`（入口与服务层共用召回） |
| `l1` | `generate_l1_summaries` / `L1Summarizer` / `L1SummarizerConfig` |
| `retriever` | `Retriever` / `RetrieverConfig` / `SearchRequest` / `L1DocView` / `L2DocView` |
| `keyword` | `KeywordService` / 归一化器 / 词池（倒排 + 语义层） |
| `chat` | `build_system_prompt` / `load_narrative_material` / `load_examples_for_input`（Prompt 装配与素材） |
| `prompt` | `PROMPT_TEMPLATE_VERSION` / 分层装配（`layers`）/ 模板构建（`builder`）/ 注入防护 |
| `event` | 事件提取器 / TopicBatcher / 降级兜底与 paraphrase |
| `bm25` / `rrf` / `decay` / `vector` | 分词与 BM25、融合、衰减、向量索引工具 |
| `behavior` / `fact` / `style` / `inference` / `example` / `utt` | 行为、知识、风格、画像推断、示例、话语块 |
| `job` / `rebuild` / `init` | 任务管理、索引重建、初始化 |

## 相邻契约

- 依赖：`ramaria-core`、`ramaria-storage`；LLM/嵌入仅经 trait 注入（**不依赖具体 provider**）。
- 被依赖：`ramaria-service`、CLI 探针（例外登记：允许直连算法原语）。
- **禁止**：硬编码 DeepSeek/OpenAI/LM Studio、网络依赖、UI 概念。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 修改检索融合 / 排序 | `src/rrf.rs`、`src/recall.rs` | 融合规则属决策保护范围，改动须先登记决策 |
| 修改衰减公式 | `src/decay.rs` | 参数改动须实验依据（决策基线 §18） |
| L1 摘要策略 | `src/l1/` | `[l1.progressive]` 配置 + 封存用例（service） |
| 事件提取 | `src/event/` + `src/inference/` | L2/L3 调度（`ramaria-service/src/lifecycle/l2_l3.rs`） |
| Prompt 段/模板 | `src/chat.rs` + `src/prompt/` | `PROMPT_TEMPLATE_VERSION` 递增（参与缓存 key）；消融门禁快照（service `suites`） |
| 关键词池与别名 | `src/keyword/` | keyword 相关表 + 服务层 keyword 用例（CLI / 桌面共用） |
| 全新检索通道 | 需先立项（决策基线 §19 候选池） | — |

## 验证

```bash
cargo test -j 2 -p ramaria-memory
```
