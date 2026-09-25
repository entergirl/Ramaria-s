# ramaria-llm

> 定位：LLM 与嵌入 provider 实现层——LM Studio / DeepSeek / OpenAI 客户端、SSE 流式、keychain、原生 safetensors 嵌入。
> 上游 SSOT：`../../../docs/dev/00-architecture/arch-decisions-unified.md`（§1.2 能力 / §18 参数）；代码级现状见 `../../../docs/architecture-ai-agent.md`。
> 状态：导航骨架（结构收敛期建立，收口阶段定稿）。

## 职责

- **LLM 客户端**：三个 provider 的统一 `LlmProvider` 实现（请求构造、错误映射、能力描述）。
- **流式解析**：SSE 增量解析为 `StreamDelta` 流（`transport`）。
- **密钥管理**：API key 只经 OS keychain（`keychain`），不落配置文件、不进日志。
- **嵌入实现**：原生 safetensors（candle）推理；模型加载、校验与（收敛后）下载 / 目录管理。
- **响应缓存接入**：可注入 `LlmResponseCache`（精确缓存，key 含模板版本）。

## 公共入口

| 模块 | 内容 |
|------|------|
| `provider` | provider 公共面（错误映射 / 能力构造等辅助） |
| `lm_studio` / `deepseek` / `openai` | 三个 `LlmProvider` 实现（`new` / `with_cache`） |
| `transport` | HTTP/SSE 传输与增量解析 |
| `keychain` | `Keychain`（OS 凭证存取） |
| `embedding` | 原生嵌入 provider（`native::create_native_provider_with_device` 等）与设备选择 |

## 相邻契约

- 依赖：`ramaria-core`（trait / 类型 / 错误）。
- 被依赖：`ramaria-service`（装配引擎）、CLI / 桌面（经 service）、探针（例外登记）。
- **禁止**：UI 依赖；不得把 provider 选择写死在 memory 层（由服务层装配注入）。
- **安全约束**：日志不含 API key 与 LLM 原始响应全文；线上 provider 按 `provider + base_url` 做隐私确认（服务层/入口层协作）。

## 常见改动落点

| 改动 | 落点 | 连带 |
|------|------|------|
| 新增 provider | `src/<provider>.rs` + `provider` 注册 | `ramaria-core::types::LlmProvider` 枚举 + 服务层装配分支（`build_llm_provider`） |
| 调整 SSE 解析 | `src/transport` | 流式用例测试（增量序列断言） |
| 嵌入模型 / 设备 | `src/embedding/` | `[embedding]` 配置 + 服务层 `restore_embedding` |
| keychain 行为 | `src/keychain.rs` | 决策基线安全约束；测试不写真实凭证 |

## 验证

```bash
cargo test -j 2 -p ramaria-llm
```
