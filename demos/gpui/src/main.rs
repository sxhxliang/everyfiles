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

use std::borrow::Cow;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use everyfiles::core::arc_swap::ArcSwap;
use everyfiles::engine::{UpdateStats, Updater};
use everyfiles::query::{self, Query, SortKey};
use everyfiles::scan::VolumeId;
use everyfiles::store::SaveMeta;
use everyfiles::watch::PollOutcome;
use everyfiles::{
    BackendDef, BackendOptions, Id, Index, OpenOptions, OpenOutcome, ResumeToken, Scanner,
    WatchEvent, Watcher, host_volume_config,
};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{
    Animation, AnimationExt as _, App, Application, AssetSource, Bounds, ClipboardItem, Context,
    CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    Focusable, FontWeight, GlobalElementId, InspectorElementId, KeyBinding, LayoutId, PaintQuad,
    Pixels, ShapedLine, SharedString, Style, TextRun, TitlebarOptions, Transformation,
    UTF16Selection, UnderlineStyle, Window, WindowBounds, WindowOptions, actions, div, fill, hsla,
    percentage, point, prelude::*, px, relative, rgb, rgba, size, svg, uniform_list,
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
        Reindex,
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

/// The one icon this demo draws. gpui loads SVGs through an [`AssetSource`]
/// and tints them with the element's text colour, so a stroke icon is enough.
/// Shape: Lucide `refresh-cw` (ISC).
const REFRESH_ICON: &str = "icons/refresh.svg";
const REFRESH_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/></svg>"##;

/// Serves the embedded icon; everything else is "not found".
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let found = path == REFRESH_ICON;
        if std::env::var_os("EVERYFILES_DEMO_TRACE").is_some() {
            eprintln!(
                "trace: asset {path:?} -> {}",
                if found { "served" } else { "missing" }
            );
        }
        Ok(found.then(|| Cow::Borrowed(REFRESH_SVG.as_bytes())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![REFRESH_ICON.into()])
    }
}

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

    /// Clamp a byte range into the current content, on char boundaries. The
    /// platform IME keeps its own idea of the text and can send a range that
    /// no longer fits (right after `reset`, for instance); these handlers are
    /// called from Objective-C, where a panic aborts the process.
    fn clamp(&self, range: Range<usize>) -> Range<usize> {
        let len = self.content.len();
        let mut start = range.start.min(len);
        while !self.content.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = range.end.min(len).max(start);
        while !self.content.is_char_boundary(end) {
            end -= 1;
        }
        start..end.max(start)
    }

    /// The text to search for. While a Chinese IME is composing, the marked
    /// text carries its syllable separators (`pin'yin`); drop those so the
    /// list already follows the pinyin before anything is committed.
    fn query(&self) -> String {
        match &self.marked_range {
            Some(r) => {
                let r = self.clamp(r.clone());
                let mut q = String::with_capacity(self.content.len());
                q.push_str(&self.content[..r.start]);
                q.extend(self.content[r.clone()].chars().filter(|&c| c != '\''));
                q.push_str(&self.content[r.end..]);
                q
            }
            None => self.content.to_string(),
        }
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
        let range = self.clamp(self.range_from_utf16(&range_utf16));
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
        let range = self.clamp(range);

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
        let range = self.clamp(range);

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        // The new selection is relative to the replaced range's start.
        let selected = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selected_range = self.clamp(selected);
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
        let range = self.clamp(self.range_from_utf16(&range_utf16));
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
// Live updates: the backend's watcher runs on its own thread, applies each
// batch to an `Updater`, publishes a snapshot and tells the window. ⌘R asks
// that thread for a directory-mtime sweep, the same catch-up the engine does
// after an event overflow, instead of a full rescan.
// ---------------------------------------------------------------------------

/// Record counts a batch or sweep changed.
#[derive(Clone, Copy, Debug)]
struct Delta {
    added: u64,
    removed: u64,
    updated: u64,
}

impl Delta {
    fn between(before: UpdateStats, after: UpdateStats) -> Self {
        Self {
            added: after.added - before.added,
            removed: after.removed - before.removed,
            updated: after.updated - before.updated,
        }
    }
}

/// What the watch thread tells the window.
enum Notice {
    Started {
        active: &'static str,
        swept: Option<Delta>,
    },
    Batch {
        events: usize,
        overflow: bool,
        delta: Delta,
    },
    Swept {
        delta: Delta,
        dirs: u64,
        elapsed: Duration,
    },
    Failed(String),
}

/// The window's side of the watch thread.
#[derive(Clone)]
struct WatchHandle {
    published: Arc<ArcSwap<Index>>,
    sweep_requested: Arc<AtomicBool>,
}

/// Everything the watch thread owns.
struct WatchSetup {
    index: Index,
    scanner: Box<dyn Scanner>,
    watcher: Box<dyn Watcher>,
    token: Option<ResumeToken>,
    root: PathBuf,
    index_path: PathBuf,
    vid: VolumeId,
}

/// Coalesce events for this long before applying a batch.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// How long one idle poll waits; bounds the latency of a ⌘R request.
const POLL: Duration = Duration::from_millis(250);
/// Persist (with the resume token) at least this often while events flow.
const SAVE_EVERY: Duration = Duration::from_secs(10);

fn spawn_watch(setup: WatchSetup, handle: WatchHandle, tx: UnboundedSender<Notice>) {
    std::thread::Builder::new()
        .name("everyfiles-watch".into())
        .spawn(move || {
            if let Err(e) = watch_thread(setup, &handle, &tx) {
                let _ = tx.unbounded_send(Notice::Failed(e.to_string()));
            }
        })
        .expect("spawn the watch thread");
}

fn watch_thread(
    setup: WatchSetup,
    handle: &WatchHandle,
    tx: &UnboundedSender<Notice>,
) -> everyfiles::Result<()> {
    let WatchSetup {
        index,
        scanner,
        mut watcher,
        token,
        root,
        index_path,
        vid,
    } = setup;
    let trace = std::env::var_os("EVERYFILES_DEMO_TRACE").is_some();
    let mut updater = Updater::new(index, scanner.as_ref());
    let active = watcher.probe(&root).map(|p| p.active).unwrap_or("?");
    let replaying = match watcher.start(&root, token.as_ref()) {
        Ok(()) => watcher.capabilities().resumable && token.is_some(),
        Err(everyfiles::Error::TokenStale) => {
            watcher.start(&root, None)?;
            false
        }
        Err(e) => return Err(e),
    };
    // Without a resume token the saved index may have missed changes made
    // while nothing was watching; a directory-mtime sweep catches up.
    let swept = (!replaying).then(|| sweep(&mut updater));
    publish(handle, &updater);
    if trace {
        eprintln!("trace: watcher {active} started, replaying={replaying}, swept={swept:?}");
    }
    let _ = tx.unbounded_send(Notice::Started {
        active,
        swept: swept.map(|(d, _, _)| d),
    });

    let mut events: Vec<WatchEvent> = Vec::new();
    let mut last_save = Instant::now();
    loop {
        if handle.sweep_requested.swap(false, Ordering::AcqRel) {
            let (delta, dirs, elapsed) = sweep(&mut updater);
            publish(handle, &updater);
            persist(&updater, watcher.as_ref(), &index_path, &vid)?;
            last_save = Instant::now();
            if trace {
                eprintln!("trace: sweep {delta:?} dirs={dirs} in {elapsed:.2?}");
            }
            let _ = tx.unbounded_send(Notice::Swept {
                delta,
                dirs,
                elapsed,
            });
        }
        let first = watcher.poll(POLL, &mut events)?;
        if first == PollOutcome::Closed {
            break;
        }
        if events.is_empty() {
            if last_save.elapsed() >= SAVE_EVERY {
                persist(&updater, watcher.as_ref(), &index_path, &vid)?;
                last_save = Instant::now();
            }
            continue;
        }
        let deadline = Instant::now() + DEBOUNCE;
        while Instant::now() < deadline {
            match watcher.poll(
                deadline.saturating_duration_since(Instant::now()),
                &mut events,
            )? {
                PollOutcome::Events(_) => {}
                PollOutcome::Timeout | PollOutcome::Closed => break,
            }
        }
        let before = updater.stats;
        let count = events.len();
        let mut overflow = false;
        for ev in events.drain(..) {
            match ev {
                WatchEvent::Path { path, deep, .. } => updater.reconcile_path(&path, deep),
                WatchEvent::Overflow | WatchEvent::RescanNeeded => {
                    overflow = true;
                    updater.sweep();
                }
            }
        }
        let compacted = updater.maybe_compact(10_000);
        publish(handle, &updater);
        if last_save.elapsed() >= SAVE_EVERY || compacted {
            persist(&updater, watcher.as_ref(), &index_path, &vid)?;
            last_save = Instant::now();
        }
        let delta = Delta::between(before, updater.stats);
        if trace {
            eprintln!("trace: batch {count} events {delta:?} overflow={overflow}");
        }
        let _ = tx.unbounded_send(Notice::Batch {
            events: count,
            overflow,
            delta,
        });
    }
    persist(&updater, watcher.as_ref(), &index_path, &vid)?;
    Ok(())
}

fn sweep(updater: &mut Updater<'_>) -> (Delta, u64, Duration) {
    let t0 = Instant::now();
    let before = updater.stats;
    updater.sweep();
    let after = updater.stats;
    (
        Delta::between(before, after),
        after.dirs_listed - before.dirs_listed,
        t0.elapsed(),
    )
}

fn publish(handle: &WatchHandle, updater: &Updater<'_>) {
    handle.published.store(Arc::new(updater.snapshot()));
}

fn persist(
    updater: &Updater<'_>,
    watcher: &dyn Watcher,
    path: &std::path::Path,
    vid: &VolumeId,
) -> everyfiles::Result<()> {
    let token = watcher.resume_token();
    everyfiles::store::save(
        updater.index(),
        path,
        &SaveMeta {
            volume_id: vid,
            resume_token: &token.0,
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The window: header with the pinyin toggle, the search box, status, results,
// and the re-index button.
// ---------------------------------------------------------------------------

struct Row {
    name: SharedString,
    dir: SharedString,
    is_dir: bool,
}

/// What `main` opened, handed to the view.
struct Opened {
    index: Arc<Index>,
    source: String,
    root: PathBuf,
    index_path: PathBuf,
    backend: &'static BackendDef,
    watch: Option<(WatchHandle, UnboundedReceiver<Notice>)>,
}

struct SearchDemo {
    index: Arc<Index>,
    root: SharedString,
    source: SharedString,
    live: SharedString,
    root_path: PathBuf,
    index_path: PathBuf,
    backend: &'static BackendDef,
    watch: Option<WatchHandle>,
    reindexing: bool,
    input: Entity<TextInput>,
    pinyin: bool,
    rows: Vec<Row>,
    hits: usize,
    elapsed: Duration,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl SearchDemo {
    fn new(opened: Opened, cx: &mut Context<Self>) -> Self {
        let input = cx.new(TextInput::new);
        // Every edit (typing, IME composition, paste, clear) notifies the
        // input; re-run the query on each.
        cx.observe(&input, |this, _, cx| this.run_search(cx))
            .detach();
        let (watch, rx) = match opened.watch {
            Some((handle, rx)) => (Some(handle), Some(rx)),
            None => (None, None),
        };
        if let Some(mut rx) = rx {
            cx.spawn(async move |this, cx| {
                while let Some(notice) = rx.next().await {
                    if this
                        .update(cx, |this, cx| this.apply_notice(notice, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        let root = opened.index.root_path().to_string_lossy().into_owned();
        let mut this = Self {
            index: opened.index,
            root: root.into(),
            source: opened.source.into(),
            live: if watch.is_some() {
                "正在启动监听…".into()
            } else {
                "没有监听器，改动需要 ⌘R".into()
            },
            root_path: opened.root,
            index_path: opened.index_path,
            backend: opened.backend,
            watch,
            reindexing: false,
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

    /// A message from the watch thread: take the newest snapshot, say what
    /// changed, and re-run the query so new names show up by themselves.
    fn apply_notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
        if let Some(w) = &self.watch {
            self.index = w.published.load_full();
        }
        self.live = match notice {
            Notice::Started { active, swept } => match swept {
                Some(d) => format!(
                    "监听中（{active}），启动补扫 +{} -{} ~{}",
                    d.added, d.removed, d.updated
                ),
                None => format!("监听中（{active}），已回放上次保存后的事件"),
            },
            Notice::Batch {
                events,
                overflow,
                delta,
            } => format!(
                "监听中，最近一批 {events} 个事件：+{} -{} ~{}{}",
                delta.added,
                delta.removed,
                delta.updated,
                if overflow {
                    "（队列溢出，已补扫）"
                } else {
                    ""
                }
            ),
            Notice::Swept {
                delta,
                dirs,
                elapsed,
            } => {
                self.reindexing = false;
                format!(
                    "重新索引：列了 {dirs} 个目录，+{} -{} ~{}，{elapsed:.2?}",
                    delta.added, delta.removed, delta.updated
                )
            }
            Notice::Failed(e) => {
                self.reindexing = false;
                self.watch = None;
                format!("监听停止：{e}；⌘R 现在做全量重扫")
            }
        }
        .into();
        self.run_search(cx);
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let needle = self.input.read(cx).query();
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

    /// With a watcher running, ask it for a directory-mtime sweep: it re-lists
    /// every directory whose mtime moved, which is what a stale index needs
    /// and far cheaper than a rescan. Without one, rescan on a background
    /// thread (the same `engine::open` with `force_rescan`, which also saves).
    fn reindex(&mut self, _: &Reindex, _: &mut Window, cx: &mut Context<Self>) {
        if self.reindexing {
            return;
        }
        self.reindexing = true;
        cx.notify();
        if let Some(w) = &self.watch {
            w.sweep_requested.store(true, Ordering::Release);
            return;
        }
        let root = self.root_path.clone();
        let index_path = self.index_path.clone();
        let backend = self.backend;
        let task = cx.background_executor().spawn(async move {
            let t0 = Instant::now();
            let scanner = (backend.make_scanner)(&BackendOptions::default())?;
            let opts = OpenOptions {
                force_rescan: true,
                ..OpenOptions::default()
            };
            let (index, _) = everyfiles::engine::open(
                scanner.as_ref(),
                &root,
                &index_path,
                host_volume_config(),
                &opts,
            )?;
            Ok::<_, everyfiles::Error>((index, t0.elapsed()))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.reindexing = false;
                match result {
                    Ok((index, elapsed)) => {
                        this.index = Arc::new(index);
                        this.live = format!("全量重扫用了 {elapsed:.2?}").into();
                        this.run_search(cx);
                    }
                    Err(e) => {
                        this.error = Some(format!("重新索引失败：{e}").into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// The bottom-right button: the refresh icon, spinning while work runs.
    fn reindex_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.reindexing;
        let icon = svg()
            .size_4()
            .flex_none()
            .path(REFRESH_ICON)
            .text_color(if busy { rgb(ACCENT) } else { rgb(TEXT) });
        let icon = if busy {
            icon.with_animation(
                "reindex-spin",
                Animation::new(Duration::from_secs(1)).repeat(),
                |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
            )
            .into_any_element()
        } else {
            icon.into_any_element()
        };
        div()
            .id("reindex")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .bg(rgb(PANEL))
            .hover(|s| s.bg(rgb(BORDER)))
            .child(icon)
            .child(div().text_xs().child(if busy {
                "正在重新索引…"
            } else {
                "重新索引　⌘R"
            }))
            .on_click(cx.listener(|this, _, window, cx| this.reindex(&Reindex, window, cx)))
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
            .on_action(cx.listener(Self::reindex))
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
            .child(div().text_xs().text_color(rgb(DIM)).child(self.live.clone()))
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(div().flex_1().truncate().text_xs().text_color(rgb(DIM)).child(
                        "输入即搜索，文件系统变动自动更新 · ⌘P 切换拼音 · ⌘R 重新索引 · esc 清空 · ⌘Q 退出 · 例：pysousuo → 拼音搜索，cq → 重庆",
                    ))
                    .child(self.reindex_button(cx)),
            )
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
    let token = match &outcome {
        OpenOutcome::Loaded { resume_token, .. } if !resume_token.is_empty() => {
            Some(ResumeToken(resume_token.clone()))
        }
        _ => None,
    };
    let source = match outcome {
        OpenOutcome::Loaded { .. } => format!("加载已保存的索引用了 {:.2?}", t0.elapsed()),
        OpenOutcome::Rescanned { reason, .. } => {
            format!("重新扫描（{reason}）用了 {:.2?}", t0.elapsed())
        }
    };
    eprintln!("everyfiles demo: {} records, {source}", index.live_count());

    // The watcher keeps the index current while the window is open. Its
    // thread owns the scanner; the window only ever sees published snapshots.
    let vid = scanner
        .volume_id(&root)
        .unwrap_or_else(|_| VolumeId::none());
    let snapshot = Arc::new(index.clone());
    let watch = match (backend.make_watcher)(&BackendOptions::default()) {
        Ok(watcher) => {
            let handle = WatchHandle {
                published: Arc::new(ArcSwap::new(snapshot.clone())),
                sweep_requested: Arc::new(AtomicBool::new(false)),
            };
            let (tx, rx) = unbounded();
            spawn_watch(
                WatchSetup {
                    index,
                    scanner,
                    watcher,
                    token,
                    root: root.clone(),
                    index_path: index_path.clone(),
                    vid,
                },
                handle.clone(),
                tx,
            );
            Some((handle, rx))
        }
        Err(e) => {
            eprintln!("everyfiles demo: no watcher ({e}); changes need ⌘R");
            None
        }
    };
    let opened = Opened {
        index: snapshot,
        source,
        root,
        index_path,
        backend,
        watch,
    };

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
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
                KeyBinding::new("cmd-r", Reindex, None),
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
                    |_, cx| cx.new(|cx| SearchDemo::new(opened, cx)),
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
