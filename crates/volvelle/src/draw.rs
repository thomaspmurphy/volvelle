//! Rendering. The viewer owns layout: atoms take their natural height,
//! lists, tables and markdown shrink first when space runs out, and runs of
//! buttons are laid out as a row. A spec that still does not fit is drawn
//! off-screen at full height and the body scrolls to keep focus in view.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Gauge, List, ListItem, ListState, Paragraph, Row,
    StatefulWidget, Table, Widget, Wrap,
};
use volvelle_schema::{Align, Direction, InputNode, Node, Tone};

use crate::app::{self, App, Field};
use crate::markdown;

pub mod palette {
    use ratatui::style::{Color, Style};

    pub fn muted() -> Style {
        Style::new().fg(Color::DarkGray)
    }
    pub fn accent() -> Style {
        Style::new().fg(Color::Cyan)
    }
    pub fn code() -> Style {
        Style::new().fg(Color::Yellow)
    }
}

fn tone(t: Tone) -> Style {
    let color = match t {
        Tone::Normal => return Style::new(),
        Tone::Muted => Color::DarkGray,
        Tone::Accent => Color::Cyan,
        Tone::Success => Color::Green,
        Tone::Warning => Color::Yellow,
        Tone::Error => Color::Red,
    };
    Style::new().fg(color)
}

/// Shrink-first nodes never go below this height.
const MIN_FLEX: u16 = 3;
/// A multiline input grows with its text between these many rows.
const TEXT_ROWS: (u16, u16) = (3, 8);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [body, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    app.scroll.page = body.height;
    let Some(spec) = &app.spec else {
        let [_, mid, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        frame.render_widget(
            Paragraph::new("volvelle · waiting for a spec")
                .style(palette::muted())
                .alignment(Alignment::Center),
            mid,
        );
        frame.render_widget(status_line(app), status);
        return;
    };
    let (total, cap) = plan(spec, body.width, body.height, app);
    let area = Rect::new(0, 0, body.width, total);
    let mut canvas = Canvas::new(area, cap);
    node(&mut canvas, area, spec, app);

    let target = canvas.focus.filter(|_| !app.scroll.manual).map(|r| {
        // At either end of the focus order, show the rest of that end too.
        let (mut start, mut end) = (r.y, r.bottom());
        if app.focus == 0 && end <= body.height {
            start = 0;
        }
        if app.focus + 1 == app.order.len() && total - start <= body.height {
            end = total;
        }
        (start as usize, (end - start) as usize)
    });
    let offset = follow(
        app.scroll.offset as usize,
        target,
        body.height as usize,
        total as usize,
    ) as u16;
    let rows = body.height.min(total - offset);
    let out = frame.buffer_mut();
    for y in 0..rows {
        for x in 0..body.width {
            out[(body.x + x, body.y + y)] = canvas.buf[(x, offset + y)].clone();
        }
    }
    if let Some(p) = canvas.cursor
        && (offset..offset + rows).contains(&p.y)
    {
        frame.set_cursor_position((body.x + p.x, body.y + p.y - offset));
    }
    app.scroll.offset = offset;
    for (id, t) in canvas.tops {
        if let Some(Field::Text { top, .. }) = app.fields.get_mut(&id) {
            *top = t;
        }
    }

    let more = match (offset > 0, offset + rows < total) {
        (true, true) => "↑↓ more ",
        (true, false) => "↑ more ",
        (false, true) => "↓ more ",
        (false, false) => "",
    };
    let [hint, right] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(Line::from(more).width() as u16),
    ])
    .areas(status);
    frame.render_widget(status_line(app), hint);
    frame.render_widget(Span::styled(more, palette::muted()), right);
}

fn status_line(app: &App) -> Line<'static> {
    let hint = |s: &'static str| Span::styled(s, palette::muted());
    if app.notice.is_some() {
        Line::styled(
            " ✓ already sent · waiting for the agent",
            Style::new().fg(Color::Yellow).bold(),
        )
    } else if app.answered {
        Line::from(vec![
            Span::styled(" ✓ sent", Style::new().fg(Color::Green)),
            hint(" · waiting for the agent"),
        ])
    } else if matches!(
        app.focused_field(),
        Some(Field::Text {
            multiline: true,
            ..
        })
    ) {
        Line::from(hint(
            " tab next · shift-tab back · enter newline · esc cancel",
        ))
    } else if app.awaiting_input() {
        Line::from(hint(
            " tab next · shift-tab back · space toggle · enter confirm · esc cancel",
        ))
    } else {
        Line::from(hint(" ctrl-c close"))
    }
}

/// Chooses the height to lay the tree out at, and the cap on focusable
/// leaves. A spec that fits takes its natural height. One that fits once
/// shrink-first nodes give way fills the pane. Anything taller keeps natural
/// heights, caps each focusable leaf at the pane so it can be shown whole,
/// and scrolls.
fn plan(spec: &Node, width: u16, view: u16, app: &App) -> (u16, u16) {
    let natural = height(spec, width, app, u16::MAX);
    if natural <= view {
        return (natural, u16::MAX);
    }
    if shrunk(spec, width, app) <= view {
        return (view, u16::MAX);
    }
    let cap = view.max(MIN_FLEX);
    (height(spec, width, app, cap), cap)
}

/// Scroll offset that shows `target` (start and length) in a window of
/// `view` rows over `total`, moving as little as possible from `offset`.
/// A target taller than the window shows its start.
fn follow(offset: usize, target: Option<(usize, usize)>, view: usize, total: usize) -> usize {
    let mut offset = offset;
    if let Some((start, len)) = target {
        offset = offset.max((start + len).saturating_sub(view)).min(start);
    }
    offset.min(total.saturating_sub(view))
}

/// Natural height of a node at the given width, with focusable leaves no
/// taller than `cap`.
fn height(n: &Node, width: u16, app: &App, cap: u16) -> u16 {
    let h = match n {
        Node::Box(b) => {
            let chrome = if bordered(b) { 2 } else { 0 };
            let inner = width.saturating_sub(chrome);
            let body = match b.direction {
                Direction::Column => slots(&b.children)
                    .iter()
                    .map(|s| match s {
                        Slot::One(c) => height(c, inner, app, cap),
                        Slot::Buttons(_) => 1,
                    })
                    .sum(),
                Direction::Row => {
                    let widths = row_widths(inner, b.children.len());
                    b.children
                        .iter()
                        .zip(widths)
                        .map(|(c, w)| height(c, w, app, cap))
                        .max()
                        .unwrap_or(0)
                }
            };
            body + chrome
        }
        Node::Text(t) => wrapped(&text_lines(&t.text), width),
        Node::Markdown(m) => wrapped(&markdown::lines(&m.text), width),
        Node::Table(t) => t.rows.len() as u16 + 3,
        Node::List(l) => l.items.len().max(1) as u16 + 2,
        Node::Input(i) if i.multiline => {
            let lines = match app.fields.get(&i.id) {
                Some(Field::Text { value, .. }) => value.split('\n').count(),
                _ => i.value.split('\n').count(),
            };
            (lines as u16).clamp(TEXT_ROWS.0, TEXT_ROWS.1) + 2
        }
        Node::Input(_) => 3,
        Node::Select(s) => s.options.len() as u16 + 2,
        Node::Toggle(_) | Node::Button(_) | Node::Progress(_) | Node::Divider(_) => 1,
    };
    if n.id().is_some() { h.min(cap) } else { h }
}

/// Height of a node once every shrink-first node inside it has given way,
/// mirroring [`column`].
fn shrunk(n: &Node, width: u16, app: &App) -> u16 {
    let full = |c: &Node, w: u16| height(c, w, app, u16::MAX);
    match n {
        Node::Box(b) => {
            let chrome = if bordered(b) { 2 } else { 0 };
            let inner = width.saturating_sub(chrome);
            let body = match b.direction {
                Direction::Column => slots(&b.children)
                    .iter()
                    .map(|s| match s {
                        Slot::One(c) if flexible(c) => full(c, inner).min(MIN_FLEX),
                        Slot::One(c) => full(c, inner),
                        Slot::Buttons(_) => 1,
                    })
                    .sum(),
                Direction::Row => {
                    let widths = row_widths(inner, b.children.len());
                    b.children
                        .iter()
                        .zip(widths)
                        .map(|(c, w)| shrunk(c, w, app))
                        .max()
                        .unwrap_or(0)
                }
            };
            body + chrome
        }
        n if flexible(n) => full(n, width).min(MIN_FLEX),
        n => full(n, width),
    }
}

fn flexible(n: &Node) -> bool {
    match n {
        Node::Table(_) | Node::List(_) | Node::Markdown(_) => true,
        Node::Box(b) => b.children.iter().any(flexible),
        _ => false,
    }
}

fn bordered(b: &volvelle_schema::BoxNode) -> bool {
    b.border || b.title.is_some()
}

fn text_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|l| Line::from(l.to_string()))
        .collect()
}

fn wrapped(lines: &[Line], width: u16) -> u16 {
    let width = width.max(1) as usize;
    lines
        .iter()
        .map(|l| l.width().max(1).div_ceil(width) as u16)
        .sum()
}

fn row_widths(width: u16, n: usize) -> Vec<u16> {
    if n == 0 {
        return Vec::new();
    }
    let area = Rect::new(0, 0, width, 1);
    Layout::horizontal(vec![Constraint::Fill(1); n])
        .spacing(1)
        .split(area)
        .iter()
        .map(|r| r.width)
        .collect()
}

enum Slot<'a> {
    One(&'a Node),
    Buttons(Vec<&'a Node>),
}

fn slots(children: &[Node]) -> Vec<Slot<'_>> {
    let mut out: Vec<Slot> = Vec::new();
    for c in children {
        match (c, out.last_mut()) {
            (Node::Button(_), Some(Slot::Buttons(run))) => run.push(c),
            (Node::Button(_), _) => out.push(Slot::Buttons(vec![c])),
            _ => out.push(Slot::One(c)),
        }
    }
    out
}

/// An off-screen surface as tall as the laid-out tree. Drawing records where
/// the focused node and the cursor land, so the body can scroll to them.
struct Canvas {
    buf: Buffer,
    /// Focusable leaves are no taller than this.
    cap: u16,
    focus: Option<Rect>,
    cursor: Option<Position>,
    /// First visible line of each text field, written back to the app.
    tops: Vec<(String, usize)>,
}

impl Canvas {
    fn new(area: Rect, cap: u16) -> Self {
        Canvas {
            buf: Buffer::empty(area),
            cap,
            focus: None,
            cursor: None,
            tops: Vec::new(),
        }
    }

    fn render_widget(&mut self, widget: impl Widget, area: Rect) {
        widget.render(area, &mut self.buf);
    }

    fn render_stateful_widget<W: StatefulWidget>(
        &mut self,
        widget: W,
        area: Rect,
        state: &mut W::State,
    ) {
        widget.render(area, &mut self.buf, state);
    }

    fn set_cursor_position(&mut self, position: impl Into<Position>) {
        self.cursor = Some(position.into());
    }
}

fn node(f: &mut Canvas, area: Rect, n: &Node, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if n.id().is_some_and(|id| app.is_focused(id)) {
        f.focus = Some(area);
    }
    match n {
        Node::Box(b) => {
            let inner = if bordered(b) {
                let mut block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(palette::muted());
                if let Some(t) = &b.title {
                    block = block.title(Span::styled(format!(" {t} "), Style::new().bold()));
                }
                let inner = block.inner(area);
                f.render_widget(block, area);
                inner
            } else {
                area
            };
            match b.direction {
                Direction::Column => column(f, inner, &b.children, app),
                Direction::Row => {
                    let rects = Layout::horizontal(vec![Constraint::Fill(1); b.children.len()])
                        .spacing(1)
                        .split(inner);
                    for (c, r) in b.children.iter().zip(rects.iter()) {
                        node(f, *r, c, app);
                    }
                }
            }
        }
        Node::Text(t) => {
            let align = match t.align {
                Align::Left => Alignment::Left,
                Align::Center => Alignment::Center,
                Align::Right => Alignment::Right,
            };
            let mut style = tone(t.tone);
            if t.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            let p = Paragraph::new(Text::from(text_lines(&t.text)))
                .style(style)
                .alignment(align)
                .wrap(Wrap { trim: false });
            f.render_widget(p, area);
        }
        Node::Markdown(m) => {
            f.render_widget(
                Paragraph::new(markdown::lines(&m.text)).wrap(Wrap { trim: false }),
                area,
            );
        }
        Node::Table(t) => {
            let widths: Vec<Constraint> = match &t.widths {
                Some(w) => w.iter().map(|&w| Constraint::Fill(w)).collect(),
                None => vec![Constraint::Fill(1); t.headers.len()],
            };
            let header = Row::new(t.headers.iter().map(|h| Cell::from(h.clone())))
                .style(Style::new().bold().fg(Color::Cyan));
            let rows = t
                .rows
                .iter()
                .map(|r| Row::new(r.iter().map(|c| Cell::from(c.clone()))));
            let table = Table::new(rows, widths)
                .header(header)
                .column_spacing(2)
                .block(frame_block(t.title.as_deref(), false));
            f.render_widget(table, area);
        }
        Node::List(l) => {
            let focused = l.id.as_deref().is_some_and(|id| app.is_focused(id));
            let items: Vec<ListItem> = l
                .items
                .iter()
                .map(|i| {
                    let mut spans = vec![Span::raw(i.label().to_string())];
                    if let Some(d) = i.detail() {
                        spans.push(Span::styled(format!("  {d}"), palette::muted()));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let mut list = List::new(items).block(frame_block(l.title.as_deref(), focused));
            let mut state = ListState::default();
            if let Some(Field::List { cursor, .. }) =
                l.id.as_ref().and_then(|id| app.fields.get(id))
            {
                state.select(Some(*cursor));
                let marker = if focused {
                    palette::accent().add_modifier(Modifier::REVERSED)
                } else {
                    Style::new().add_modifier(Modifier::BOLD)
                };
                list = list.highlight_style(marker).highlight_symbol("› ");
            }
            f.render_stateful_widget(list, area, &mut state);
        }
        Node::Input(i) => {
            let focused = app.is_focused(&i.id);
            let block = frame_block(i.label.as_deref(), focused);
            let inner = block.inner(area);
            f.render_widget(block, area);
            if let Some(Field::Text {
                value, cursor, top, ..
            }) = app.fields.get(&i.id)
            {
                text_field(f, inner, i, value, *cursor, *top, focused);
            }
        }
        Node::Select(s) => {
            let focused = app.is_focused(&s.id);
            let block = frame_block(s.label.as_deref(), focused);
            let Some(Field::Choice { cursor, chosen, .. }) = app.fields.get(&s.id) else {
                return;
            };
            let items: Vec<ListItem> = s
                .options
                .iter()
                .map(|o| {
                    let on = chosen.iter().any(|c| c == o.value());
                    let mark = match (s.multi, on) {
                        (true, true) => "[x] ",
                        (true, false) => "[ ] ",
                        (false, true) => "◉ ",
                        (false, false) => "○ ",
                    };
                    let mut spans = vec![Span::raw(mark), Span::raw(o.label().to_string())];
                    if let Some(d) = o.detail() {
                        spans.push(Span::styled(format!("  {d}"), palette::muted()));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let mut list = List::new(items).block(block);
            let mut state = ListState::default().with_selected(Some(*cursor));
            if focused {
                list = list.highlight_style(palette::accent().add_modifier(Modifier::BOLD));
            }
            f.render_stateful_widget(list, area, &mut state);
        }
        Node::Toggle(t) => {
            let on = matches!(app.fields.get(&t.id), Some(Field::Toggle(true)));
            let style = if app.is_focused(&t.id) {
                palette::accent().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let mark = if on { "[x] " } else { "[ ] " };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(mark),
                    Span::raw(t.label.clone()),
                ]))
                .style(style),
                area,
            );
        }
        Node::Button(_) => buttons(f, area, &[n], app),
        Node::Progress(p) => {
            let pct = (p.value * 100.0).round();
            let label = match &p.label {
                Some(l) => format!("{l} {pct}%"),
                None => format!("{pct}%"),
            };
            let color = tone(p.tone).fg.unwrap_or(Color::Cyan);
            let gauge = Gauge::default()
                .ratio(p.value.clamp(0.0, 1.0))
                .label(label)
                .gauge_style(Style::new().fg(color).bg(Color::Black));
            f.render_widget(gauge, Rect { height: 1, ..area });
        }
        Node::Divider(d) => {
            let mut block = Block::new()
                .borders(Borders::TOP)
                .border_style(palette::muted());
            if let Some(t) = &d.title {
                block = block.title(Span::styled(format!(" {t} "), palette::muted()));
            }
            f.render_widget(block, area);
        }
    }
}

/// The text of an input. It scrolls sideways to the cursor's column and,
/// when multiline, down to its line.
fn text_field(
    f: &mut Canvas,
    area: Rect,
    i: &InputNode,
    value: &str,
    cursor: usize,
    top: usize,
    focused: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (line, col) = app::line_col(value, cursor);
    let rows = area.height as usize;
    let top = follow(top, Some((line, 1)), rows, value.split('\n').count());
    f.tops.push((i.id.clone(), top));
    let skip = (col + 1).saturating_sub(area.width as usize);
    let lines: Vec<Line> = if value.is_empty() {
        vec![Line::styled(
            i.placeholder.clone().unwrap_or_default(),
            palette::muted(),
        )]
    } else {
        value
            .split('\n')
            .skip(top)
            .take(rows)
            .map(|l| {
                let shown: String = if i.secret {
                    "•".repeat(l.chars().count())
                } else {
                    l.to_string()
                };
                Line::from(shown.chars().skip(skip).collect::<String>())
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines), area);
    if focused {
        f.set_cursor_position((area.x + (col - skip) as u16, area.y + (line - top) as u16));
    }
}

fn frame_block(title: Option<&str>, focused: bool) -> Block<'static> {
    let border = if focused {
        palette::accent()
    } else {
        palette::muted()
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border);
    if let Some(t) = title {
        let style = if focused {
            palette::accent().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        block = block.title(Span::styled(format!(" {t} "), style));
    }
    block
}

/// Lays children top to bottom at natural height. When they overflow,
/// shrink-first children give up space from the bottom up; anything that
/// still does not fit is clipped.
fn column(f: &mut Canvas, area: Rect, children: &[Node], app: &App) {
    let slots = slots(children);
    let mut heights: Vec<u16> = slots
        .iter()
        .map(|s| match s {
            Slot::One(c) => height(c, area.width, app, f.cap),
            Slot::Buttons(_) => 1,
        })
        .collect();
    let mut over = heights.iter().sum::<u16>().saturating_sub(area.height);
    for (slot, h) in slots.iter().zip(heights.iter_mut()).rev() {
        if over == 0 {
            break;
        }
        if let Slot::One(c) = slot
            && flexible(c)
        {
            let give = over.min(h.saturating_sub(MIN_FLEX));
            *h -= give;
            over -= give;
        }
    }
    let mut y = area.y;
    let bottom = area.y + area.height;
    for (slot, h) in slots.iter().zip(heights) {
        if y >= bottom {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, h.min(bottom - y));
        match slot {
            Slot::One(c) => node(f, rect, c, app),
            Slot::Buttons(run) => buttons(f, rect, run, app),
        }
        y += h;
    }
}

fn buttons(f: &mut Canvas, area: Rect, run: &[&Node], app: &App) {
    if run
        .iter()
        .any(|n| n.id().is_some_and(|id| app.is_focused(id)))
    {
        f.focus = Some(Rect { height: 1, ..area });
    }
    let mut spans = Vec::new();
    for (i, n) in run.iter().enumerate() {
        let Node::Button(b) = n else { continue };
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let base = tone(b.tone);
        let span = if app.is_focused(&b.id) {
            let bg = base.fg.unwrap_or(Color::Cyan);
            Span::styled(
                format!("[ {} ]", b.label),
                Style::new().fg(Color::Black).bg(bg).bold(),
            )
        } else {
            Span::styled(format!("[ {} ]", b.label), base)
        };
        spans.push(span);
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn app(spec: serde_json::Value) -> App {
        let mut app = App::default();
        app.render("t".into(), volvelle_schema::parse_spec(spec).unwrap());
        app
    }

    fn screen(term: &mut Terminal<TestBackend>, app: &mut App) -> Vec<String> {
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// Twelve toggles, then a row holding an input and a select, then buttons.
    fn long() -> serde_json::Value {
        let mut children: Vec<_> = (0..12).map(|i| json!({"type": "toggle", "id": format!("t{i}"), "label": format!("toggle {i}")})).collect();
        children.push(json!({"type": "box", "direction": "row", "children": [
            {"type": "input", "id": "name", "label": "Name"},
            {"type": "select", "id": "env", "label": "Env", "options": ["dev", "prod"]}
        ]}));
        children.push(json!({"type": "button", "id": "ok", "label": "OK"}));
        children.push(json!({"type": "button", "id": "no", "label": "No"}));
        json!({"type": "box", "title": "Long", "children": children})
    }

    #[test]
    fn follow_moves_only_as_needed() {
        assert_eq!(follow(0, Some((3, 2)), 10, 40), 0);
        assert_eq!(follow(0, Some((12, 3)), 10, 40), 5);
        assert_eq!(follow(5, Some((8, 1)), 10, 40), 5);
        assert_eq!(follow(5, Some((2, 1)), 10, 40), 2);
        assert_eq!(follow(0, Some((20, 15)), 10, 40), 20);
        assert_eq!(follow(35, None, 10, 40), 30);
        assert_eq!(follow(7, None, 10, 4), 0);
    }

    #[test]
    fn plan_shrinks_before_it_scrolls() {
        let a = app(json!({"type": "box", "children": [
            {"type": "list", "items": ["a", "b", "c", "d", "e", "f"]},
            {"type": "toggle", "id": "x", "label": "X"}
        ]}));
        let spec = a.spec.as_ref().unwrap();
        assert_eq!(plan(spec, 20, 20, &a), (9, u16::MAX));
        assert_eq!(plan(spec, 20, 5, &a), (5, u16::MAX));
        assert_eq!(plan(spec, 20, 3, &a), (9, 3));
    }

    #[test]
    fn overflow_scrolls_to_keep_focus_visible() {
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut a = app(long());
        let key = |a: &mut App, code, n| (0..n).for_each(|_| _ = a.key(KeyEvent::from(code)));
        let rows = screen(&mut term, &mut a);
        assert!(rows[0].contains("Long") && rows[1].contains("toggle 0"));
        assert!(rows[9].ends_with("↓ more "));

        key(&mut a, KeyCode::BackTab, 1);
        let rows = screen(&mut term, &mut a);
        assert!(rows[7].contains("[ OK ]  [ No ]"), "{rows:#?}");
        assert!(rows[8].starts_with("╰"), "last field shows the end");
        assert!(rows[9].ends_with("↑ more "));
        let offset = a.scroll.offset;

        key(&mut a, KeyCode::BackTab, 4);
        let rows = screen(&mut term, &mut a);
        assert_eq!(a.scroll.offset, offset, "focus moved within view");
        assert!(rows[0].contains("toggle 9"));

        key(&mut a, KeyCode::BackTab, 4);
        let rows = screen(&mut term, &mut a);
        assert!(rows[0].contains("toggle 7"));
        assert!(rows[9].ends_with("↑↓ more "));

        key(&mut a, KeyCode::PageUp, 3);
        let rows = screen(&mut term, &mut a);
        assert_eq!(a.scroll.offset, 0);
        assert!(rows[9].ends_with("↓ more "));

        key(&mut a, KeyCode::Tab, 1);
        let rows = screen(&mut term, &mut a);
        assert!(
            rows[8].contains("toggle 8"),
            "a key brings focus back into view"
        );
    }

    #[test]
    fn long_example_overflows_a_30_row_pane() {
        let mut a = app(serde_json::from_str(include_str!("../../../examples/long.json")).unwrap());
        let (total, cap) = plan(a.spec.as_ref().unwrap(), 80, 29, &a);
        assert!(total > 29 && cap == 29);
        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        a.focus = a.order.iter().position(|id| id == "body").unwrap();
        let rows = screen(&mut term, &mut a);
        assert!(rows.iter().any(|r| r.contains("Description")));
        assert!(rows[29].contains("enter newline"));
    }

    #[test]
    fn multiline_input_scrolls_to_its_cursor() {
        let text: Vec<String> = (1..=12).map(|i| format!("line {i}")).collect();
        let mut a =
            app(json!({"type": "input", "id": "m", "multiline": true, "value": text.join("\n")}));
        let mut term = Terminal::new(TestBackend::new(30, 12)).unwrap();
        let rows = screen(&mut term, &mut a);
        assert!(
            rows[1].contains("line 5") && rows[8].contains("line 12"),
            "{rows:#?}"
        );
        assert_eq!(term.get_cursor_position().unwrap(), Position::new(8, 8));
        for _ in 0..7 {
            a.key(KeyEvent::from(KeyCode::Up));
        }
        let rows = screen(&mut term, &mut a);
        assert!(rows[1].contains("line 5"), "cursor stayed in view");
        a.key(KeyEvent::from(KeyCode::Up));
        let rows = screen(&mut term, &mut a);
        assert!(rows[1].contains("line 4"));
    }
}
