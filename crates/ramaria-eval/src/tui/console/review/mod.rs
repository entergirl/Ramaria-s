//! crates/ramaria-eval/src/tui/console/review/mod.rs - Ramaria 评测台审查屏状态
//!
//! 设计特点:
//! - 运行列表只读 `run.json`：一级运行与 `oracle/` 子运行都列出，坏文件跳过并汇总
//! - 审查屏按需读取：索引只存题目位置，选中一题才读该行完整记录
//! - 焦点两段：题单负责选题，详情负责滚动，条件切换在两者之上
//! - 原对话缓存跨运行保留：同一语料只解析一次，回投时只更新缓存
//! - 任何读取失败写状态消息，不把审查屏整体切到错误页

use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;

use crate::corpus::Sample;

mod detail;
mod index;
mod render;
mod runs;
mod source;

#[cfg(test)]
mod tests;

pub(crate) use detail::{DetailLine, DetailStyle, DetailView, build_detail};
pub(crate) use index::{ConditionEntry, ItemRef, ReviewIndex};
pub(crate) use render::{draw_review, draw_runs, draw_source};
pub(crate) use runs::RunsState;
pub(crate) use source::{CorpusRequest, SourceLine};

// =========================================================
// 审查屏
// =========================================================

/// 审查屏焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewFocus {
    /// 左侧题单
    List,
    /// 右侧详情
    Detail,
}

/// 已解析的语料缓存（按语料名 / 版本 / 语言区分）。
pub(crate) struct CorpusCache {
    pub(crate) key: String,
    pub(crate) samples: Vec<Sample>,
}

/// 原对话片段屏状态。
///
/// 字段约定:
/// - `loading`: 后台是否仍在读语料；
/// - `error`: 加载或取片段失败的原因（含期望路径）；
/// - `sample_key`: 片段所属样本；
/// - `lines`: 片段行；
/// - `scroll`: 纵向滚动偏移；
/// - `max_scroll`: 上一帧算出的最大滚动量（渲染回填）。
pub(crate) struct SourceView {
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) sample_key: String,
    pub(crate) lines: Vec<SourceLine>,
    pub(crate) scroll: u16,
    pub(crate) max_scroll: Cell<u16>,
}

impl SourceView {
    /// 开始加载。
    fn loading(sample_key: String) -> Self {
        Self {
            loading: true,
            error: None,
            sample_key,
            lines: Vec::new(),
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }

    /// 片段就绪。
    fn ready(sample_key: String, lines: Vec<SourceLine>) -> Self {
        Self {
            loading: false,
            error: None,
            sample_key,
            lines,
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }

    /// 加载或取片段失败。
    fn failed(sample_key: String, message: String) -> Self {
        Self {
            loading: false,
            error: Some(message),
            sample_key,
            lines: Vec::new(),
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }
}

/// 审查屏状态。
///
/// 字段约定:
/// - `eval_root` / `root` / `index`: 评测根、当前运行根与只读索引；
/// - `loading` / `error`: 索引构建状态；
/// - `condition_pos` / `question_pos`: 条件与题号位置（题号在当前条件内）；
/// - `focus` / `detail` / `detail_scroll`: 焦点、当前题详情与纵向偏移；
/// - `source`: 原对话片段屏状态；
/// - `corpus`: 跨运行保留的语料缓存；
/// - `detail_max_scroll`: 上一帧算出的详情最大滚动量（渲染回填，按键据此钳制）；
/// - `status`: 单次读取失败等提示，不改变当前屏。
pub(crate) struct ReviewState {
    pub(crate) eval_root: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) index: Option<ReviewIndex>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) condition_pos: usize,
    pub(crate) question_pos: usize,
    pub(crate) focus: ReviewFocus,
    pub(crate) detail: Option<DetailView>,
    pub(crate) detail_scroll: u16,
    pub(crate) source: Option<SourceView>,
    pub(crate) corpus: Option<CorpusCache>,
    pub(crate) detail_max_scroll: Cell<u16>,
    pub(crate) status: Option<String>,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            eval_root: PathBuf::new(),
            root: PathBuf::new(),
            index: None,
            loading: false,
            error: None,
            condition_pos: 0,
            question_pos: 0,
            focus: ReviewFocus::List,
            detail: None,
            detail_scroll: 0,
            source: None,
            corpus: None,
            detail_max_scroll: Cell::new(0),
            status: None,
        }
    }
}

/// 打开原对话的判定结果。
pub(crate) enum SourceOpen {
    /// 已用缓存构建片段，可直接切屏
    Ready,
    /// 需要后台加载语料
    Load(CorpusRequest),
    /// 当前没有可打开的详情
    None,
}

impl ReviewState {
    /// 进入审查：清空上次运行的索引与片段，保留语料缓存。
    ///
    /// 参数:
    /// - `run_root`: 运行根（含 `run.json`）；
    /// - `eval_root`: 评测根（语料相对它解析）。
    pub(crate) fn begin(&mut self, run_root: PathBuf, eval_root: PathBuf) {
        self.eval_root = eval_root;
        self.root = run_root;
        self.index = None;
        self.loading = true;
        self.error = None;
        self.condition_pos = 0;
        self.question_pos = 0;
        self.focus = ReviewFocus::List;
        self.detail = None;
        self.detail_scroll = 0;
        self.detail_max_scroll.set(0);
        self.source = None;
        self.status = None;
    }

    /// 索引就绪：选中第一个有题的条件并读第一题。
    pub(crate) fn apply_loaded(&mut self, index: ReviewIndex) {
        self.loading = false;
        self.error = None;
        self.status = None;
        self.condition_pos = index
            .conditions()
            .iter()
            .position(|condition| !index.questions(&condition.id).is_empty())
            .unwrap_or(0);
        self.index = Some(index);
        self.question_pos = 0;
        self.detail_scroll = 0;
        self.detail_max_scroll.set(0);
        self.focus = ReviewFocus::List;
        let no_questions = self.index.as_ref().is_some_and(|index| {
            index
                .conditions()
                .iter()
                .all(|condition| index.questions(&condition.id).is_empty())
        });
        if no_questions {
            self.error = Some("该运行没有逐题产物".to_string());
            return;
        }
        self.refresh_detail();
    }

    /// 索引构建失败。
    pub(crate) fn fail(&mut self, message: String) {
        self.loading = false;
        self.index = None;
        self.detail = None;
        self.error = Some(message);
    }

    /// 当前条件。
    pub(crate) fn condition(&self) -> Option<&ConditionEntry> {
        let index = self.index.as_ref()?;
        index.conditions().get(self.condition_pos)
    }

    /// 当前条件下的题单位置下标。
    pub(crate) fn questions(&self) -> &[usize] {
        match (self.index.as_ref(), self.condition()) {
            (Some(index), Some(condition)) => index.questions(&condition.id),
            _ => &[],
        }
    }

    /// 当前题在 `items` 里的下标。
    fn current_global(&self) -> Option<usize> {
        self.questions().get(self.question_pos).copied()
    }

    /// 当前题的位置条目。
    pub(crate) fn current_ref(&self) -> Option<&ItemRef> {
        let global = self.current_global()?;
        self.index.as_ref()?.item(global)
    }

    /// 切换条件：跳过没有题的条件；全部为空时不动。
    pub(crate) fn select_condition(&mut self, delta: isize) {
        let Some(index) = self.index.as_ref() else {
            return;
        };
        let total = index.conditions().len();
        if total == 0 {
            return;
        }
        let mut pos = self.condition_pos;
        for _ in 0..total {
            pos = if delta >= 0 {
                (pos + 1) % total
            } else {
                (pos + total - 1) % total
            };
            if !index.questions(&index.conditions()[pos].id).is_empty() {
                break;
            }
        }
        if pos != self.condition_pos {
            self.condition_pos = pos;
            self.question_pos = 0;
            self.detail_scroll = 0;
            self.refresh_detail();
        }
    }

    /// 在当前条件内移动题号（越界钳制）。
    pub(crate) fn select_question(&mut self, delta: isize) {
        let len = self.questions().len();
        if len == 0 {
            return;
        }
        let next = if delta >= 0 {
            (self.question_pos + delta as usize).min(len - 1)
        } else {
            self.question_pos.saturating_sub((-delta) as usize)
        };
        if next != self.question_pos {
            self.question_pos = next;
            self.detail_scroll = 0;
            self.refresh_detail();
        }
    }

    /// 跳到第一题。
    pub(crate) fn first_question(&mut self) {
        self.select_question(-(self.question_pos as isize));
    }

    /// 跳到最后一题。
    pub(crate) fn last_question(&mut self) {
        let len = self.questions().len();
        if len > 0 {
            self.select_question((len - 1) as isize - self.question_pos as isize);
        }
    }

    /// 切换焦点。
    pub(crate) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            ReviewFocus::List => ReviewFocus::Detail,
            ReviewFocus::Detail => ReviewFocus::List,
        };
    }

    /// 滚动详情：上界取上一帧渲染算出的最大值。
    pub(crate) fn scroll_detail(&mut self, delta: isize) {
        let max = self.detail_max_scroll.get();
        let next = if delta >= 0 {
            self.detail_scroll.saturating_add(delta as u16)
        } else {
            self.detail_scroll.saturating_sub((-delta) as u16)
        };
        self.detail_scroll = next.min(max);
    }

    /// 详情滚到底。
    pub(crate) fn scroll_detail_to_end(&mut self) {
        self.detail_scroll = self.detail_max_scroll.get();
    }

    /// 题单可见窗口；只按窗口返回下标区间。
    pub(crate) fn window(&self, height: usize) -> Range<usize> {
        let len = self.questions().len();
        let height = height.max(1);
        if len <= height {
            return 0..len;
        }
        let start = if self.question_pos < height {
            0
        } else {
            self.question_pos + 1 - height
        };
        start..(start + height).min(len)
    }

    /// 读取当前题并重建详情；读取失败写状态消息。
    pub(crate) fn refresh_detail(&mut self) {
        let Some(global) = self.current_global() else {
            self.detail = None;
            return;
        };
        let Some(index) = self.index.as_ref() else {
            return;
        };
        match index.read_item(global) {
            Ok(item) => match index.read_scored(global) {
                Ok(scored) => {
                    self.detail = Some(build_detail(item, scored));
                    self.status = None;
                }
                Err(error) => {
                    self.detail = Some(build_detail(item, None));
                    self.status = Some(format!("读判分结果失败: {error}"));
                }
            },
            Err(error) => {
                self.detail = None;
                self.status = Some(format!("读逐题产物失败: {error}"));
            }
        }
    }

    /// 打开当前题的原对话片段：有缓存直接构建，否则返回加载请求。
    pub(crate) fn open_source(&mut self) -> SourceOpen {
        let Some(detail) = self.detail.as_ref() else {
            return SourceOpen::None;
        };
        let sample_key = detail.item.sample_key.clone();
        let evidence = detail.item.evidence.clone();
        if evidence.is_empty() {
            self.source = Some(SourceView::ready(sample_key, source::no_evidence_lines()));
            return SourceOpen::Ready;
        }
        let request = CorpusRequest {
            root: self.eval_root.clone(),
            name: detail.item.conditions.corpus_name.clone(),
            version: detail.item.conditions.corpus_version.clone(),
            language: detail.item.conditions.language.clone(),
        };
        if let Some(cache) = self
            .corpus
            .as_ref()
            .filter(|cache| cache.key == request.key())
        {
            let view = match source::build_snippet(&cache.samples, &sample_key, &evidence) {
                Ok(lines) => SourceView::ready(sample_key, lines),
                Err(message) => SourceView::failed(sample_key, message),
            };
            self.source = Some(view);
            return SourceOpen::Ready;
        }
        self.source = Some(SourceView::loading(sample_key));
        SourceOpen::Load(request)
    }

    /// 应用语料加载结果：更新缓存；只有口径与当前题一致时才构建片段。
    ///
    /// 参数:
    /// - `key`: 语料名 / 版本 / 语言；
    /// - `result`: 全部样本或错误文本。
    ///
    /// 说明: 换运行、换题后到达的旧结果只更新缓存，不写当前片段；
    /// 成功但样本已变时清空等待中的片段，避免把旧对话显示到新题上。
    pub(crate) fn apply_corpus(&mut self, key: String, result: Result<Vec<Sample>, String>) {
        let current = self.current_corpus_key();
        if current.as_deref() != Some(key.as_str()) {
            return;
        }
        let pending = self
            .source
            .as_ref()
            .filter(|source| source.loading)
            .map(|source| source.sample_key.clone());
        match result {
            Ok(samples) => {
                self.corpus = Some(CorpusCache { key, samples });
                let Some(sample_key) = pending else {
                    return;
                };
                let current_sample = self
                    .detail
                    .as_ref()
                    .map(|detail| detail.item.sample_key.as_str());
                if current_sample != Some(sample_key.as_str()) {
                    self.source = None;
                    return;
                }
                let evidence = self
                    .detail
                    .as_ref()
                    .map(|detail| detail.item.evidence.clone())
                    .unwrap_or_default();
                let view = match self.corpus.as_ref() {
                    Some(cache) => {
                        match source::build_snippet(&cache.samples, &sample_key, &evidence) {
                            Ok(lines) => SourceView::ready(sample_key, lines),
                            Err(message) => SourceView::failed(sample_key, message),
                        }
                    }
                    None => SourceView::failed(sample_key, "语料缓存丢失".to_string()),
                };
                self.source = Some(view);
            }
            Err(message) => {
                if let Some(sample_key) = pending {
                    let current = self
                        .detail
                        .as_ref()
                        .map(|detail| detail.item.sample_key == sample_key);
                    if current == Some(true) {
                        self.source = Some(SourceView::failed(sample_key, message));
                    } else {
                        self.source = None;
                    }
                }
            }
        }
    }

    /// 当前题的语料键（名 / 版本 / 语言）。
    fn current_corpus_key(&self) -> Option<String> {
        self.detail.as_ref().map(|detail| {
            format!(
                "{}/{}/{}",
                detail.item.conditions.corpus_name,
                detail.item.conditions.corpus_version,
                detail.item.conditions.language
            )
        })
    }
}
