//! 单行输入框，设置窗口里加词用。照 GPUI 自带的输入框示例改的：光标按字移动，支持输入法组字、鼠标选择、
//! 复制粘贴。回车是 `Submit`，输入框自己不处理，交给外面的界面。

use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, EntityInputHandler,
    FocusHandle, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, actions, div, fill, point, px, relative, size,
};

actions!(
    text_field,
    [Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut, Copy, Submit]
);

/// 输入框的按键只在这个上下文里生效，不影响窗口别处的快捷键。
const CONTEXT: &str = "TextField";
const HEIGHT: f32 = 28.0;
const CURSOR_WIDTH: f32 = 1.5;

/// 输入框的按键，开窗口前绑一次。`secondary` 在 macOS 上是 ⌘，别的系统是 Ctrl。
pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("secondary-left", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("secondary-right", End, context),
        KeyBinding::new("secondary-v", Paste, context),
        KeyBinding::new("secondary-x", Cut, context),
        KeyBinding::new("secondary-c", Copy, context),
        KeyBinding::new("enter", Submit, context),
    ]);
}

/// 输入框的颜色，跟着设置窗口的深浅色和高亮色，由外面的界面每次画之前设好。
#[derive(Clone, Copy, Default)]
pub struct FieldColors {
    pub text: Hsla,
    pub placeholder: Hsla,
    /// 光标、选中的底色和聚焦时的边框。
    pub accent: Hsla,
    pub background: Hsla,
    pub border: Hsla,
}

pub struct TextField {
    focus: FocusHandle,
    content: String,
    placeholder: SharedString,
    pub colors: FieldColors,
    /// 选中的范围，按字节算。没选中时是光标的位置。
    selected: Range<usize>,
    /// 光标在选中范围的开头（从后往前选的）。
    reversed: bool,
    /// 输入法正在组的字。
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// 按着鼠标在选字。
    selecting: bool,
    /// 字比输入框长时往左滚了多少，光标总在看得见的地方。
    scroll: Pixels,
}

impl TextField {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        TextField {
            focus: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            colors: FieldColors::default(),
            selected: 0..0,
            reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
            scroll: px(0.),
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content.clear();
        self.selected = 0..0;
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.next_boundary(self.selected.end), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.previous_boundary(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.next_boundary(self.cursor()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace(['\n', '\r', '\t'], " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else { return 0 };
        if self.content.is_empty() || position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + self.scroll)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content[..offset].char_indices().next_back().map_or(0, |(at, _)| at)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content[offset..].chars().next().map_or(offset, |c| offset + c.len_utf8())
    }

    /// 输入法按 UTF-16 算下标，这里按字节。
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        for (at, c) in self.content.char_indices() {
            if utf16 >= offset {
                return at;
            }
            utf16 += c.len_utf16();
        }
        self.content.len()
    }

    /// 输入法要换掉的那一段：它指明了就用它，否则是正在组的字，再否则是选中的。
    fn target_range(&self, range_utf16: Option<&Range<usize>>) -> Range<usize> {
        range_utf16.map(|range| self.range_from_utf16(range)).or(self.marked.clone()).unwrap_or(self.selected.clone())
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].chars().map(char::len_utf16).sum()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.target_range(range_utf16.as_ref());
        self.content.replace_range(range.clone(), new_text);
        self.selected = range.start + new_text.len()..range.start + new_text.len();
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.target_range(range_utf16.as_ref());
        self.content.replace_range(range.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        // 输入法给的选中范围是相对组字那一段的
        let start = self.offset_to_utf16(range.start);
        self.selected = match new_selected_range_utf16 {
            Some(selected) => self.range_from_utf16(&(start + selected.start..start + selected.end)),
            None => range.start + new_text.len()..range.start + new_text.len(),
        };
        self.reversed = false;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let left = bounds.left() - self.scroll;
        Some(Bounds::from_corners(
            point(left + layout.x_for_index(range.start), bounds.top()),
            point(left + layout.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let index = self.last_layout.as_ref()?.index_for_x(local.x + self.scroll)?;
        (!self.content.is_empty()).then(|| self.offset_to_utf16(index.min(self.content.len())))
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .h(px(HEIGHT))
            .px(px(8.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(6.0))
            .border_1()
            .border_color(if focused { self.colors.accent } else { self.colors.border })
            .bg(self.colors.background)
            .text_size(px(13.0))
            .line_height(px(18.0))
            .text_color(self.colors.text)
            .child(TextElement { field: cx.entity() })
    }
}

/// 画字、光标和选中的底色，并把输入法要的位置报给窗口。
struct TextElement {
    field: gpui::Entity<TextField>,
}

struct Prepainted {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
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
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let field = self.field.read(cx);
        let style = window.text_style();
        let (text, color) = if field.content.is_empty() {
            (field.placeholder.clone(), field.colors.placeholder)
        } else {
            (SharedString::from(field.content.clone()), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // 输入法正在组的字加下划线
        let runs = match &field.marked {
            Some(marked) => [
                TextRun { len: marked.start, ..run.clone() },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.0), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: text.len() - marked.end, ..run },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(text, font_size, &runs, None);
        // 字比输入框长时往左滚，让光标留在看得见的地方；删短了再滚回来
        let cursor_x = if field.content.is_empty() { px(0.) } else { line.x_for_index(field.cursor()) };
        let room = bounds.size.width - px(CURSOR_WIDTH);
        let scroll = field.scroll.min(cursor_x).max(cursor_x - room).min((line.width - room).max(px(0.))).max(px(0.));
        let x = |offset: usize| bounds.left() - scroll + line.x_for_index(offset);
        let (cursor, selection) = if field.selected.is_empty() {
            let at = point(bounds.left() - scroll + cursor_x, bounds.top());
            (Some(fill(Bounds::new(at, size(px(CURSOR_WIDTH), bounds.size.height)), field.colors.accent)), None)
        } else {
            let selection = fill(
                Bounds::from_corners(
                    point(x(field.selected.start), bounds.top()),
                    point(x(field.selected.end), bounds.bottom()),
                ),
                field.colors.accent.opacity(0.25),
            );
            (None, Some(selection))
        };
        Prepainted { line: Some(line), cursor, selection, scroll }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepainted: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.field.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.field.clone()), cx);
        let Some(line) = prepainted.line.take() else { return };
        let scroll = prepainted.scroll;
        // 滚出去的字不画到输入框的内边距上
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepainted.selection.take() {
                window.paint_quad(selection);
            }
            let _ = line.paint(point(bounds.left() - scroll, bounds.top()), window.line_height(), window, cx);
            if focus.is_focused(window)
                && let Some(cursor) = prepainted.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        self.field.update(cx, |field, _| {
            field.last_layout = Some(line);
            field.last_bounds = Some(bounds);
            field.scroll = scroll;
        });
    }
}
