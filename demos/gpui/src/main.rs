//! A small gpui front end for the everyfiles engine.
//!
//! Type and the list follows; `⌘P` toggles pinyin matching (`pysousuo` finds
//! 拼音搜索, `cq` finds 重庆, `拼音sousuo` mixes the two), `esc` clears, `⌘Q`
//! quits. Run from this directory:
//!
//! ```sh
//! cargo run --release -- ~/Documents
//! ```
//!
//! The index is the same cache file the `everyfiles` CLI uses for that root,
//! so a second launch loads it in well under a millisecond instead of
//! rescanning. The search box is adapted from gpui's own `input` example so
//! Chinese IME input works; everything else is a few `div`s.

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use everyfiles::query::{self, Query, SortKey};
use everyfiles::{BackendOptions, Id, Index, OpenOptions, OpenOutcome, host_volume_config};
use gpui::{
    App, Application, Bounds, ClipboardItem, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, FontWeight,
    GlobalElementId, InspectorElementId, KeyBinding, LayoutId, PaintQuad, Pixels, ShapedLine,
    SharedString, Style, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle, Window,
    WindowBounds, WindowOptions, actions, div, fill, hsla, point, prelude::*, px, relative, rgb,
    rgba, size, uniform_list,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    everyfiles_demo,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Copy,
        Cut,
        Clear,
        TogglePinyin,
        Quit,
    ]
);

/// Rows shown at once. The engine picks them with `select_nth`, so a query
/// that matches a million names still only sorts this many.
const PAGE: usize = 300;

const BG: u32 = 0x1e1e2e;
const WELL: u32 = 0x181825;
const PANEL: u32 = 0x313244;
const BORDER: u32 = 0x45475a;
const TEXT: u32 = 0xcdd6f4;
const DIM: u32 = 0x7f849c;
const ACCENT: u32 = 0x89b4fa;
const DIR: u32 = 0xf9e2af;
const ERR: u32 = 0xf38ba8;

// ---------------------------------------------------------------------------
// Search box: gpui's `input` example, trimmed to what a one-line query needs.
// ---------------------------------------------------------------------------

struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
}

impl TextInput {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: "输入文件名、拼音或拼音首字母，例如 pysousuo、report、cq".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
        }
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

/// The painted line of text, cursor and selection; registers the input
/// handler so the platform IME talks to the `TextInput` above.
struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), hsla(0., 0., 1., 0.3))
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    rgb(ACCENT),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(0x89b4fa40),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), window, cx)
            .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .w_full()
            .line_height(px(30.))
            .text_size(px(20.))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .h(px(30. + 10. * 2.))
                    .w_full()
                    .px_3()
                    .py(px(10.))
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(TextElement { input: cx.entity() }),
            )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ---------------------------------------------------------------------------
// The window: header with the pinyin toggle, the search box, status, results.
// ---------------------------------------------------------------------------

struct Row {
    name: SharedString,
    dir: SharedString,
    is_dir: bool,
}

struct SearchDemo {
    index: Index,
    root: SharedString,
    source: SharedString,
    input: Entity<TextInput>,
    pinyin: bool,
    rows: Vec<Row>,
    hits: usize,
    elapsed: Duration,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl SearchDemo {
    fn new(index: Index, source: String, cx: &mut Context<Self>) -> Self {
        let input = cx.new(TextInput::new);
        // Every edit (typing, IME commit, paste, clear) notifies the input;
        // re-run the query on each.
        cx.observe(&input, |this, _, cx| this.run_search(cx))
            .detach();
        let root = index.root_path().to_string_lossy().into_owned();
        let mut this = Self {
            index,
            root: root.into(),
            source: source.into(),
            input,
            pinyin: true,
            rows: Vec::new(),
            hits: 0,
            elapsed: Duration::ZERO,
            error: None,
            focus_handle: cx.focus_handle(),
        };
        this.execute(String::new());
        this
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let needle = self.input.read(cx).content.to_string();
        self.execute(needle);
        cx.notify();
    }

    /// Match, keep the first page by name, resolve just those paths.
    fn execute(&mut self, needle: String) {
        let q = Query {
            needle,
            pinyin: self.pinyin,
            ..Query::default()
        };
        let result = query::search(&self.index, &q);
        if std::env::var_os("EVERYFILES_DEMO_TRACE").is_some() {
            match &result {
                Ok(m) => eprintln!(
                    "trace: {:?} pinyin={} -> {} hits in {:.2?}",
                    q.needle,
                    q.pinyin,
                    m.ids.len(),
                    m.elapsed
                ),
                Err(e) => eprintln!("trace: {:?} pinyin={} -> error: {e}", q.needle, q.pinyin),
            }
        }
        match result {
            Ok(mut m) => {
                self.hits = m.ids.len();
                self.elapsed = m.elapsed;
                query::truncate_top(&self.index, &mut m.ids, SortKey::Name, false, PAGE);
                query::sort_ids(&self.index, &mut m.ids, SortKey::Name, false);
                self.rows = m.ids.iter().map(|&id| self.row(id)).collect();
                self.error = None;
            }
            Err(e) => {
                self.rows.clear();
                self.hits = 0;
                self.error = Some(e.to_string().into());
            }
        }
    }

    fn row(&self, id: Id) -> Row {
        Row {
            name: String::from_utf8_lossy(self.index.name(id))
                .into_owned()
                .into(),
            dir: self
                .index
                .resolve_path(self.index.parent(id))
                .to_string_lossy()
                .into_owned()
                .into(),
            is_dir: self.index.is_dir(id),
        }
    }

    fn toggle_pinyin(&mut self, _: &TogglePinyin, _: &mut Window, cx: &mut Context<Self>) {
        self.pinyin = !self.pinyin;
        self.run_search(cx);
    }

    fn clear(&mut self, _: &Clear, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.reset(cx));
        let handle = self.input.read(cx).focus_handle.clone();
        window.focus(&handle);
    }
}

impl Render for SearchDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status: SharedString = match &self.error {
            Some(e) => e.clone(),
            None => format!(
                "{} 条命中，{:.2?} · 显示前 {} 条 · 索引 {} 条记录 · {}",
                self.hits,
                self.elapsed,
                self.rows.len(),
                self.index.live_count(),
                self.source
            )
            .into(),
        };
        let status_color = if self.error.is_some() { ERR } else { DIM };
        let pinyin = self.pinyin;

        div()
            .key_context("SearchDemo")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_pinyin))
            .on_action(cx.listener(Self::clear))
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_baseline()
                            .gap_3()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("everyfiles"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(rgb(DIM))
                                    .child(self.root.clone()),
                            ),
                    )
                    .child(
                        div()
                            .id("pinyin-toggle")
                            .flex_none()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(if pinyin { rgb(ACCENT) } else { rgb(PANEL) })
                            .text_color(if pinyin { rgb(BG) } else { rgb(TEXT) })
                            .child(if pinyin {
                                "拼音匹配：开　⌘P"
                            } else {
                                "拼音匹配：关　⌘P"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_pinyin(&TogglePinyin, window, cx)
                            })),
                    ),
            )
            .child(self.input.clone())
            .child(div().text_color(rgb(status_color)).child(status))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded_lg()
                    .bg(rgb(WELL))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(
                        uniform_list(
                            "results",
                            self.rows.len(),
                            cx.processor(|this, range: Range<usize>, _window, _cx| {
                                range
                                    .map(|ix| {
                                        let row = &this.rows[ix];
                                        div()
                                            .id(ix)
                                            .flex()
                                            .flex_row()
                                            .items_baseline()
                                            .gap_3()
                                            .px_3()
                                            .py_1()
                                            .hover(|s| s.bg(rgb(PANEL)))
                                            .child(
                                                div()
                                                    .whitespace_nowrap()
                                                    .text_color(if row.is_dir {
                                                        rgb(DIR)
                                                    } else {
                                                        rgb(TEXT)
                                                    })
                                                    .child(row.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(rgb(DIM))
                                                    .child(row.dir.clone()),
                                            )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .h_full(),
                    ),
            )
            .child(div().text_xs().text_color(rgb(DIM)).child(
                "输入即搜索 · ⌘P 切换拼音 · esc 清空 · ⌘Q 退出 · 例：pysousuo → 拼音搜索，cq → 重庆，拼音sousuo 混排亦可",
            ))
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("current directory"));
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let backend = everyfiles::default_backend();
    let scanner = (backend.make_scanner)(&BackendOptions::default()).expect("backend scanner");
    let index_path = everyfiles::default_index_path(&root)
        .unwrap_or_else(|| std::env::temp_dir().join("everyfiles-gpui-demo.idx"));

    eprintln!(
        "everyfiles demo: opening {} with the {} backend",
        root.display(),
        backend.name
    );
    let t0 = Instant::now();
    let (index, outcome) = everyfiles::engine::open(
        scanner.as_ref(),
        &root,
        &index_path,
        host_volume_config(),
        &OpenOptions::default(),
    )
    .unwrap_or_else(|e| {
        eprintln!("everyfiles demo: {e}");
        std::process::exit(1)
    });
    let source = match outcome {
        OpenOutcome::Loaded { .. } => format!("加载已保存的索引用了 {:.2?}", t0.elapsed()),
        OpenOutcome::Rescanned { reason, .. } => {
            format!("重新扫描（{reason}）用了 {:.2?}", t0.elapsed())
        }
    };
    eprintln!("everyfiles demo: {} records, {source}", index.live_count());

    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, None),
            KeyBinding::new("delete", Delete, None),
            KeyBinding::new("left", Left, None),
            KeyBinding::new("right", Right, None),
            KeyBinding::new("shift-left", SelectLeft, None),
            KeyBinding::new("shift-right", SelectRight, None),
            KeyBinding::new("home", Home, None),
            KeyBinding::new("end", End, None),
            KeyBinding::new("cmd-a", SelectAll, None),
            KeyBinding::new("cmd-v", Paste, None),
            KeyBinding::new("cmd-c", Copy, None),
            KeyBinding::new("cmd-x", Cut, None),
            KeyBinding::new("escape", Clear, None),
            KeyBinding::new("cmd-p", TogglePinyin, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("everyfiles".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| SearchDemo::new(index, source, cx)),
            )
            .expect("open window");
        window
            .update(cx, |view, window, cx| {
                let handle = view.input.read(cx).focus_handle.clone();
                window.focus(&handle);
                cx.activate(true);
            })
            .expect("focus the search box");
    });
}
