# ramaria-llm

> 定位：LLM 与嵌入 provider 实现层——LM Studio / DeepSeek / OpenAI 客户端、SSE 流式、keychain、原生 safetensors 嵌入。
> 上游 SSOT：`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§1.2 能力 / §6 后端 / §18 参数）；代码级现状见 `../../../docs/architecture-ai-agent.md`。

## 职责

- **LLM 客户端**：三个 provider 的统一 `LlmProvider` 实现（请求构造、错误映射、能力描述）。
- **流式解析**：SSE 增量解析为 `StreamDelta` 流（`transport`）。
- **密钥管理**：API key 只经 OS keychain（`keychain`），不落配置文件、不进日志。
- **嵌入实现**：原生 safetensors（candle）推理；模型加载与校验（`embedding/`，feature `embedding-native`）。
- **模型文件管理**：下载（断点续传）/ SHA-256 校验 / 目录管理（`model_manager`）。
- **响应缓存接入**：可注入 `LlmResponseCache`（精确缓存，key 含模板版本）。

## 文件地图（目录 → 职责）

| 路径 | 职责 | 测试位置 |
|------|------|----------|
| `src/lib.rs` | crate 根：模块声明与公共项 | — |
| `src/lm_studio.rs` / `src/deepseek.rs` / `src/openai.rs` | 三个 `LlmProvider` 实现（`new` / `with_cache`） | 内联 |
| `src/provider/` | provider 公共面：`base`（能力构造）/ `request`（请求构造）/ `retry`（退避）/ `macros`（三 provider 共用样板） | `src/provider/tests.rs` |
| `src/transport/` | OpenAI-compatible 传输：`client`（HTTP）/ `sse`（增量解析）/ `error`（错误映射） | `src/transport/tests.rs` |
| `src/keychain.rs` | `Keychain`（Windows 凭据管理器存取；service `ramaria`） | 内联 |
| `src/model_manager/` | 模型文件生命周期：`presets`（`MODEL_PRESETS` / `DEFAULT_MODEL_ID`）/ `download`（断点续传）/ `manager` / `fs`（校验与目录） | `src/model_manager/tests.rs` |
| `src/embedding/` | 嵌入 provider：`native`（safetensors + candle 推理）/ `noop`（降级占位）/ `models/`（`bert` / `llama` / 各模型头维适配）/ `onnx/`（**已停用旧后端**，无 feature 启用、不承诺可编译、计划移除） | `src/embedding/native/tests.rs`、`src/embedding/models/llama_head_dim/tests.rs` |
| `tests/` | 集成测试：`embedding_tests`（原生嵌入）/ `model_manager_tests`（下载与校验）/ `dp_cluster_tuning` / `qwen3_embed_local_verify`（本地验证，忽略态） | `tests/` |

## 公共入口

| 模块 | 内容 |
|------|------|
| `provider` | provider 公共面（错误映射 / 能力构造等辅助） |
| `lm_studio` / `deepseek` / `openai` | 三个 `LlmProvider` 实现（`new` / `with_cache`） |
| `transport` | HTTP/SSE 传输与增量解析 |
| `keychain` | `Keychain`（OS 凭证存取） |
| `embedding` | `native::create_native_provider_with_device` 等原生嵌入入口与设备选择 |
| `model_manager` | `ModelManager` / `MODEL_PRESETS` / `DEFAULT_MODEL_ID` / `DownloadProgress`（下载 / 校验 / 目录管理） |

## 相邻契约

- 依赖：`ramaria-core`（trait / 类型 / 错误）。
- 被依赖：`ramaria-service`（引擎装配与模型用例）；CLI / 桌面经服务层使用，不直连。
- **禁止**：UI 依赖；不得把 provider 选择写死在 memory 层（由服务层装配注入）。
- **安全约束**：日志不含 API key 与 LLM 原始响应全文；线上 provider 按 `provider + base_url` 做隐私确认（服务层/入口层协作）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增 provider | `src/<provider>.rs` + `src/provider/` 公共面 | `ramaria-core::types::LlmProvider` 枚举 + 服务层装配分支（`build_llm_provider`） |
| 调整 SSE 解析 | `src/transport/sse.rs` | 流式用例测试（增量序列断言） |
| 嵌入模型 / 设备 | `src/embedding/` | `[embedding]` 配置 + 服务层模型用例（`ramaria-service/src/model.rs`） |
| 模型下载 / 校验 | `src/model_manager/` | 服务层 `model.rs` 用例编排 |
| keychain 行为 | `src/keychain.rs` | 决策基线安全约束；测试不写真实凭证 |

## 验证

```bash
cargo test -j 2 -p ramaria-llm
```
