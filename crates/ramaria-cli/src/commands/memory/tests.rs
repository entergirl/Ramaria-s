//! crates/ramaria-cli/src/commands/memory/tests.rs - memory 命令单元测试
//!
//! 设计特点:
//! - 覆盖层级别名解析（l1↔summary / l2↔events / l3↔profile）与默认 persona 回退口径。
//! - 覆盖 L2 文本块渲染：编号基数 / 列顺序 / 截断长度 / 时间列取 `start` / 小数位。
//! - 覆盖 L2 `--json` 条目表示：字段集与 ISO-8601 时间换算，非正时间输出 null。

use super::*;

/// 层级别名双支持：l1↔summary / l2↔events / l3↔profile。
#[test]
fn layer_aliases_resolve() {
    assert_eq!(resolve_layer("l1"), Some("l1"));
    assert_eq!(resolve_layer("summary"), Some("l1"));
    assert_eq!(resolve_layer("l2"), Some("l2"));
    assert_eq!(resolve_layer("events"), Some("l2"));
    assert_eq!(resolve_layer("l3"), Some("l3"));
    assert_eq!(resolve_layer("profile"), Some("l3"));
    assert_eq!(resolve_layer("l4"), None);
    assert_eq!(resolve_layer(""), None);
}

/// 默认 persona 为 rama-0001。
#[test]
fn default_persona_is_rama() {
    let args = MemoryArgs {
        layer: "l1".to_string(),
        persona: None,
        limit: 10,
        offset: 0,
        json: false,
    };
    assert_eq!(default_persona(&args), "rama-0001");
    let args_with = MemoryArgs {
        layer: "l1".to_string(),
        persona: Some("user-0001".to_string()),
        limit: 10,
        offset: 0,
        json: false,
    };
    assert_eq!(default_persona(&args_with), "user-0001");
}

/// 分页偏移转服务层口径：常规值原样，超出 u32 上限收敛到上限。
#[test]
fn offset_conversion_saturates() {
    assert_eq!(offset_as_u32(0), 0);
    assert_eq!(offset_as_u32(37), 37);
    assert_eq!(offset_as_u32(usize::MAX), u32::MAX);
}

/// 造一条 L2 事件视图（仅填展示相关字段）。
fn event_view(id: i64, title: String, summary: String, start: i64, created_at: i64) -> L2EventView {
    L2EventView {
        id,
        persona_uid: "char-0001".to_string(),
        title,
        summary,
        keywords: None,
        valence: -0.1,
        confidence: 0.8,
        presentation: ramaria_core::types::Presentation::Subjective,
        share: 0.5,
        attitude: None,
        salience: 0.6,
        created_at,
        start,
        end: start + 1_800_000,
    }
}

/// L2 文本块：编号基数 / 列顺序 / 截断长度 / 时间列取 `start` / 小数位口径。
#[test]
fn l2_text_block_renders_columns() {
    let title = "外".repeat(90);
    let summary = "内".repeat(130);
    // created_at 与 start 取不同值：时间列必须来自 start
    let items = vec![event_view(
        7,
        title.clone(),
        summary.clone(),
        1_700_000_000_000,
        1_718_006_400_000,
    )];

    let block = l2_text_block("rama-0001", &items);
    let lines: Vec<&str> = block.lines().collect();
    assert_eq!(lines[0], "", "文本块以空行开头");
    assert_eq!(lines[1], crate::ui::separator_line(), "分隔线口径不变");
    assert_eq!(lines[2], "  L2 记忆事件 — rama-0001（1 条）");
    assert_eq!(lines[3], crate::ui::separator_line());
    assert_eq!(lines[4], "");
    assert_eq!(lines[5], "  [0] #7", "编号基数与列顺序保持现状");
    assert_eq!(
        lines[6],
        crate::ui::labeled_line("标题", &crate::util::truncate(&title, 80)),
        "标题列按 80 字符截断"
    );
    assert!(lines[6].ends_with('…'), "超长标题应带省略号: {}", lines[6]);
    assert_eq!(
        lines[7],
        crate::ui::labeled_line("摘要", &crate::util::truncate(&summary, 120)),
        "摘要列按 120 字符截断"
    );
    assert_eq!(
        lines[8],
        crate::ui::labeled_line("时间", "2023-11-14 22:13"),
        "时间列取事件 start"
    );
    assert_eq!(lines[9], crate::ui::labeled_line("确凿度", "0.80"));
    assert_eq!(lines[10], crate::ui::labeled_line("显著性", "0.60"));
    assert_eq!(lines.len(), 11, "单条事件的文本块行数固定");
}

/// L2 文本块：start 非正值时省略时间行，其余展示列不受影响。
#[test]
fn l2_text_block_skips_time_when_start_invalid() {
    let items = vec![event_view(
        3,
        "短标题".to_string(),
        "短摘要".to_string(),
        0,
        1_000,
    )];

    let block = l2_text_block("rama-0001", &items);
    assert!(
        block.lines().all(|line| !line.starts_with("  时间")),
        "start 无效时不应输出时间行: {block}"
    );
    assert!(block.contains("  [0] #3"));
    assert!(block.ends_with('\n'), "文本块以换行结束");
}

/// `--json` 信封：键与条目数保持现状，条目时间为 ISO-8601 UTC。
#[test]
fn l2_json_data_envelope_keeps_shape() {
    let items = vec![event_view(
        7,
        "备考".to_string(),
        "复习到深夜".to_string(),
        1_700_000_000_000,
        1_718_006_400_000,
    )];
    let data = L2JsonData {
        layer: "l2",
        persona_uid: "char-0001",
        total: 1,
        items: l2_json_items(&items),
    };
    let json = serde_json::to_value(&data).expect("序列化应成功");

    assert_eq!(json["layer"], "l2");
    assert_eq!(json["persona_uid"], "char-0001");
    assert_eq!(json["total"], 1);
    assert_eq!(json["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["items"][0]["start"], "2023-11-14T22:13:20Z");
    assert_eq!(json["items"][0]["end"], "2023-11-14T22:43:20Z");
}

/// `--json` 条目：字段集固定 id / title / summary / keywords / valence / confidence /
/// salience / start / end（不多不少），时间戳为 ISO-8601 UTC。
#[test]
fn l2_json_items_keep_item_shape() {
    let items = vec![event_view(
        7,
        "备考".to_string(),
        "复习到深夜".to_string(),
        1_700_000_000_000,
        1_718_006_400_000,
    )];
    let json_items = l2_json_items(&items);
    assert_eq!(json_items.len(), 1);

    let item = json_items[0].as_object().expect("条目应为对象");
    let mut keys: Vec<&str> = item.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "id",
        "title",
        "summary",
        "keywords",
        "valence",
        "confidence",
        "salience",
        "start",
        "end",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected, "条目字段集不多不少");

    assert_eq!(item["id"], 7);
    assert_eq!(item["title"], "备考");
    assert_eq!(item["summary"], "复习到深夜");
    assert_eq!(item["keywords"], serde_json::Value::Null);
    assert_eq!(item["valence"], -0.1);
    assert_eq!(item["confidence"], 0.8);
    assert_eq!(item["salience"], 0.6);
    assert_eq!(item["start"], "2023-11-14T22:13:20Z");
    assert_eq!(item["end"], "2023-11-14T22:43:20Z");
}

/// `--json` 条目边界：`start` / `end` 非正（缺失口径）时输出 `null`，其余字段不受影响。
#[test]
fn l2_json_items_null_for_non_positive_times() {
    let mut item = event_view(
        3,
        "待定".to_string(),
        "摘要".to_string(),
        0,
        1_718_006_400_000,
    );
    item.end = -1;

    let json_items = l2_json_items(&[item]);
    assert_eq!(
        json_items[0]["start"],
        serde_json::Value::Null,
        "start 非正输出 null"
    );
    assert_eq!(
        json_items[0]["end"],
        serde_json::Value::Null,
        "end 非正输出 null"
    );
    assert_eq!(json_items[0]["id"], 3, "其余字段不受影响");
}
