# Ramaria 测试 Fixtures

此目录包含固定测试数据，用于记忆系统和检索系统的自动化测试。

## 文件说明

### conversations.json
7 组固定中文对话，覆盖以下场景：
- **conv-001**: 日常闲聊——周末计划和认知心理学
- **conv-002**: 技术讨论——学习Rust编程
- **conv-003**: 情绪表达——工作压力和焦虑
- **conv-004**: 知识问答——宋朝科举制度
- **conv-005**: 情绪表达——项目成功的喜悦
- **conv-006**: 人生困惑——职业转型决策
- **conv-007**: 日常分享——阅读《三体》后的讨论

每条 fixture 包含：
- `id`: 唯一标识
- `scenario`: 场景描述
- `persona_uid`: 关联的人格标识
- `messages`: 对话消息数组（user/assistant 交替）
- `expected_l1`: 预期的 L1 摘要字段（供验证用）

## 使用方式

```rust
// 加载对话 fixtures
let fixtures: serde_json::Value = serde_json::from_str(
    &std::fs::read_to_string("tests/fixtures/conversations.json")?
)?;
```

## 维护规则

- 所有对话内容为中文，反映真实对话语境
- 助手回复遵循 `||` 分隔风格（对齐 rama-0001 的 persona.toml 配置）
- 新增 fixture 时更新此 README 的清单
- 不在此目录放入任何包含 API key 或敏感信息的数据
