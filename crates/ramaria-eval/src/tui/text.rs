//! crates/ramaria-eval/src/tui/text.rs - Ramaria 评测沙盒文本缓冲
//!
//! 设计特点:
//! - 以 char 为存储单位：中文、表情按一个字符移动与删除，不产生半个字符
//! - 光标是纯 char 下标，切片与定位都按字符计算，不依赖字节边界
//! - 粘贴统一把 CRLF 与裸 CR 归一为 LF，避免缓冲里出现不可见的回车
//! - 换行插入不受限制，单行字段由调用方过滤换行

/// 多行文本缓冲。
///
/// 状态:
/// - `chars`: 文本按字符展开的序列；
/// - `cursor`: 光标位置（`0..=chars.len()`），始终落在字符边界上。
pub(crate) struct TextBuffer {
    chars: Vec<char>,
    cursor: usize,
}

impl TextBuffer {
    /// 空缓冲，光标停在起点。
    pub(crate) fn new() -> Self {
        Self {
            chars: Vec::new(),
            cursor: 0,
        }
    }

    /// 全文。
    pub(crate) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// 带光标标记的全文：在字符光标处插入 `|`，供没有真实光标定位的终端界面显示插入点。
    pub(crate) fn text_with_cursor(&self) -> String {
        let mut text = String::with_capacity(self.chars.len() + 1);
        for (index, ch) in self.chars.iter().enumerate() {
            if index == self.cursor {
                text.push('|');
            }
            text.push(*ch);
        }
        if self.cursor == self.chars.len() {
            text.push('|');
        }
        text
    }

    /// 是否没有任何字符。
    pub(crate) fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// 字符数（不是字节数）。
    pub(crate) fn char_count(&self) -> usize {
        self.chars.len()
    }

    // ===== 编辑 =====

    /// 在光标处插入一个字符，光标随之后移。
    pub(crate) fn insert_char(&mut self, value: char) {
        self.chars.insert(self.cursor, value);
        self.cursor += 1;
    }

    /// 在光标处插入一段文本；CRLF 与裸 CR 先归一为 LF。
    pub(crate) fn insert_str(&mut self, value: &str) {
        for ch in normalized(value) {
            self.insert_char(ch);
        }
    }

    /// 删除光标前一个字符；光标在起点时不做任何事。
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.chars.remove(self.cursor);
    }

    /// 删除光标处字符；光标在末尾时不做任何事。
    pub(crate) fn delete(&mut self) {
        if self.cursor >= self.chars.len() {
            return;
        }
        self.chars.remove(self.cursor);
    }

    /// 在光标处插入换行。
    pub(crate) fn newline(&mut self) {
        self.insert_char('\n');
    }

    // ===== 移动 =====

    /// 光标左移一位。
    pub(crate) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// 光标右移一位。
    pub(crate) fn move_right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    /// 光标上移一行，列位置超出上一行时贴到行尾；已在首行时回到行首。
    pub(crate) fn move_up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            self.cursor = 0;
            return;
        }
        let column = self.cursor - start;
        // start - 1 是上一行行尾的换行符
        let previous_end = start - 1;
        let previous_start = self.chars[..previous_end]
            .iter()
            .rposition(|ch| *ch == '\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.cursor = previous_start + column.min(previous_end - previous_start);
    }

    /// 光标下移一行，列位置超出下一行时贴到行尾；已在末行时跳到文本末尾。
    pub(crate) fn move_down(&mut self) {
        let end = self.line_end();
        if end >= self.chars.len() {
            self.cursor = self.chars.len();
            return;
        }
        let column = self.cursor - self.line_start();
        let next_start = end + 1;
        let next_end = self.chars[next_start..]
            .iter()
            .position(|ch| *ch == '\n')
            .map(|index| next_start + index)
            .unwrap_or(self.chars.len());
        self.cursor = next_start + column.min(next_end - next_start);
    }

    /// 光标移到当前行行首。
    pub(crate) fn home(&mut self) {
        self.cursor = self.line_start();
    }

    /// 光标移到当前行行尾。
    pub(crate) fn end(&mut self) {
        self.cursor = self.line_end();
    }

    // ===== 行定位 =====

    /// 当前行行首下标。
    fn line_start(&self) -> usize {
        self.chars[..self.cursor]
            .iter()
            .rposition(|ch| *ch == '\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    /// 当前行行尾下标（行尾换行符位置或文本末尾）。
    fn line_end(&self) -> usize {
        self.chars[self.cursor..]
            .iter()
            .position(|ch| *ch == '\n')
            .map(|index| self.cursor + index)
            .unwrap_or(self.chars.len())
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// 把 CRLF 与裸 CR 归一为 LF 后的字符序列。
fn normalized(value: &str) -> Vec<char> {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .collect()
}
