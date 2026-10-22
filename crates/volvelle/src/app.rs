//! Viewer state: the current spec, what the user has entered, and focus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Map, Value};
use volvelle_schema::{Event, Item, Node};

/// How long the status line stays emphasised after a key on a locked view.
const NOTICE: Duration = Duration::from_millis(1500);

pub enum Field {
    /// `cursor` counts chars; `top` is the first line shown when multiline.
    Text {
        value: String,
        cursor: usize,
        top: usize,
        multiline: bool,
    },
    Choice {
        cursor: usize,
        chosen: Vec<String>,
        options: Vec<String>,
        multi: bool,
    },
    Toggle(bool),
    List {
        cursor: usize,
        values: Vec<String>,
    },
    Button,
}

#[derive(Default)]
pub struct App {
    pub view: String,
    pub spec: Option<Node>,
    pub fields: HashMap<String, Field>,
    /// Focusable ids in tree order.
    pub order: Vec<String>,
    pub focus: usize,
    /// Set once the user has answered. The view locks until the next render,
    /// so one render gets exactly one answer.
    pub answered: bool,
    /// When the locked-view notice expires.
    pub notice: Option<Instant>,
    pub scroll: Scroll,
    pub quit: bool,
}

/// Vertical scroll of a body taller than the pane.
#[derive(Default)]
pub struct Scroll {
    pub offset: u16,
    /// Set by PageUp and PageDown; the next other key brings focus back into view.
    pub manual: bool,
    /// Pane height at the last draw.
    pub page: u16,
}

impl App {
    pub fn render(&mut self, view: String, spec: Node) {
        let same_view = view == self.view;
        let focused = self.focused_id().map(str::to_string);
        let mut old = std::mem::take(&mut self.fields);
        let mut order = Vec::new();
        let mut fields = HashMap::new();
        spec.walk(&mut |node| {
            let Some(id) = node.id() else { return };
            let fresh = init(node);
            let field = match old.remove(id) {
                Some(prev) if same_view => carry(prev, fresh),
                _ => fresh,
            };
            order.push(id.to_string());
            fields.insert(id.to_string(), field);
        });
        self.focus = focused
            .filter(|_| same_view)
            .and_then(|f| order.iter().position(|id| *id == f))
            .unwrap_or(0);
        if !same_view {
            self.scroll = Scroll {
                page: self.scroll.page,
                ..Scroll::default()
            };
        }
        self.view = view;
        self.order = order;
        self.fields = fields;
        self.spec = Some(spec);
        self.answered = false;
        self.notice = None;
    }

    pub fn focused_id(&self) -> Option<&str> {
        self.order.get(self.focus).map(String::as_str)
    }

    pub fn is_focused(&self, id: &str) -> bool {
        !self.answered && self.focused_id() == Some(id)
    }

    pub fn focused_field(&self) -> Option<&Field> {
        self.fields
            .get(self.focused_id()?)
            .filter(|_| !self.answered)
    }

    /// Clears an expired notice, returning true when the status line changes.
    pub fn expire(&mut self, now: Instant) -> bool {
        let expired = self.notice.is_some_and(|t| now >= t);
        if expired {
            self.notice = None;
        }
        expired
    }

    /// True while the view is waiting on the human.
    pub fn awaiting_input(&self) -> bool {
        self.spec.is_some() && !self.answered && !self.order.is_empty()
    }

    pub fn values(&self) -> Map<String, Value> {
        let mut out = Map::new();
        for id in &self.order {
            let value = match &self.fields[id] {
                Field::Text { value, .. } => Value::String(value.clone()),
                Field::Choice {
                    chosen,
                    multi: true,
                    ..
                } => Value::Array(chosen.iter().cloned().map(Value::String).collect()),
                Field::Choice { chosen, .. } => chosen
                    .first()
                    .cloned()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                Field::Toggle(on) => Value::Bool(*on),
                Field::List { .. } | Field::Button => continue,
            };
            out.insert(id.clone(), value);
        }
        out
    }

    /// Handles a key press, returning an event when the user answers.
    pub fn key(&mut self, key: KeyEvent) -> Option<Event> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        self.spec.as_ref()?;
        let step = self.scroll.page.saturating_sub(1).max(1);
        match key.code {
            KeyCode::PageUp => {
                self.scroll.manual = true;
                self.scroll.offset = self.scroll.offset.saturating_sub(step);
                return None;
            }
            KeyCode::PageDown => {
                self.scroll.manual = true;
                self.scroll.offset = self.scroll.offset.saturating_add(step);
                return None;
            }
            _ => self.scroll.manual = false,
        }
        if self.answered {
            self.notice = Some(Instant::now() + NOTICE);
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                return Some(self.answer(|view, values| Event::Cancel { view, values }));
            }
            KeyCode::Tab => return self.move_focus(1),
            KeyCode::BackTab => return self.move_focus(-1),
            _ => {}
        }
        let id = self.focused_id()?.to_string();
        let field = self.fields.get_mut(&id)?;
        match field {
            Field::Text {
                value,
                cursor,
                multiline,
                ..
            } => match key.code {
                KeyCode::Char(c) => {
                    value.insert(byte_at(value, *cursor), c);
                    *cursor += 1;
                }
                KeyCode::Enter if *multiline => {
                    value.insert(byte_at(value, *cursor), '\n');
                    *cursor += 1;
                }
                KeyCode::Backspace if *cursor > 0 => {
                    *cursor -= 1;
                    value.remove(byte_at(value, *cursor));
                }
                KeyCode::Delete if *cursor < value.chars().count() => {
                    value.remove(byte_at(value, *cursor));
                }
                KeyCode::Left => *cursor = cursor.saturating_sub(1),
                KeyCode::Right => *cursor = (*cursor + 1).min(value.chars().count()),
                KeyCode::Up if *multiline => {
                    let (line, col) = line_col(value, *cursor);
                    *cursor = if line == 0 {
                        0
                    } else {
                        char_at(value, line - 1, col)
                    };
                }
                KeyCode::Down if *multiline => {
                    let (line, col) = line_col(value, *cursor);
                    *cursor = char_at(value, line + 1, col);
                }
                KeyCode::Home if *multiline => {
                    *cursor = char_at(value, line_col(value, *cursor).0, 0)
                }
                KeyCode::End if *multiline => {
                    *cursor = char_at(value, line_col(value, *cursor).0, usize::MAX)
                }
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = value.chars().count(),
                KeyCode::Enter | KeyCode::Down => return self.move_focus(1),
                KeyCode::Up => return self.move_focus(-1),
                _ => {}
            },
            Field::Choice {
                cursor,
                chosen,
                options,
                multi,
            } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    *cursor = cursor.saturating_sub(1);
                    if !*multi {
                        *chosen = vec![options[*cursor].clone()];
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(options.len() - 1);
                    if !*multi {
                        *chosen = vec![options[*cursor].clone()];
                    }
                }
                KeyCode::Char(' ') if *multi => {
                    let v = &options[*cursor];
                    if let Some(i) = chosen.iter().position(|c| c == v) {
                        chosen.remove(i);
                    } else {
                        chosen.push(v.clone());
                    }
                }
                KeyCode::Enter => return self.move_focus(1),
                _ => {}
            },
            Field::Toggle(on) => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter => *on = !*on,
                KeyCode::Down | KeyCode::Char('j') => return self.move_focus(1),
                KeyCode::Up | KeyCode::Char('k') => return self.move_focus(-1),
                _ => {}
            },
            Field::List { cursor, values } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(values.len().saturating_sub(1))
                }
                KeyCode::Enter if !values.is_empty() => {
                    let value = values[*cursor].clone();
                    return Some(self.answer(|view, values| Event::Select {
                        view,
                        id,
                        value,
                        values,
                    }));
                }
                _ => {}
            },
            Field::Button => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    return Some(self.answer(|view, values| Event::Submit {
                        view,
                        button: id,
                        values,
                    }));
                }
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                    return self.move_focus(-1);
                }
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                    return self.move_focus(1);
                }
                _ => {}
            },
        }
        None
    }

    fn move_focus(&mut self, delta: isize) -> Option<Event> {
        let n = self.order.len() as isize;
        if n > 0 {
            self.focus = (self.focus as isize + delta).rem_euclid(n) as usize;
        }
        None
    }

    fn answer(&mut self, make: impl FnOnce(String, Map<String, Value>) -> Event) -> Event {
        self.answered = true;
        make(self.view.clone(), self.values())
    }
}

fn init(node: &Node) -> Field {
    match node {
        Node::Input(n) => Field::Text {
            cursor: n.value.chars().count(),
            value: n.value.clone(),
            top: 0,
            multiline: n.multiline,
        },
        Node::Select(n) => {
            let options: Vec<String> = n.options.iter().map(|o| o.value().to_string()).collect();
            let mut chosen = n.value.clone();
            if !n.multi && chosen.is_empty() {
                chosen = options.first().cloned().into_iter().collect();
            }
            let cursor = chosen
                .first()
                .and_then(|c| options.iter().position(|o| o == c))
                .unwrap_or(0);
            Field::Choice {
                cursor,
                chosen,
                options,
                multi: n.multi,
            }
        }
        Node::Toggle(n) => Field::Toggle(n.value),
        Node::List(n) => Field::List {
            cursor: 0,
            values: n.items.iter().map(|i| Item::value(i).to_string()).collect(),
        },
        _ => Field::Button,
    }
}

/// Keeps what the user entered when the same field survives a re-render.
fn carry(prev: Field, fresh: Field) -> Field {
    match (prev, fresh) {
        (
            Field::Text {
                value, cursor, top, ..
            },
            Field::Text { multiline, .. },
        ) => Field::Text {
            value,
            cursor,
            top,
            multiline,
        },
        (prev @ Field::Toggle(_), Field::Toggle(_)) => prev,
        (
            Field::Choice { chosen, .. },
            Field::Choice {
                options,
                multi,
                chosen: default,
                ..
            },
        ) => {
            let mut kept: Vec<String> =
                chosen.into_iter().filter(|c| options.contains(c)).collect();
            if !multi {
                kept.truncate(1);
                if kept.is_empty() {
                    kept = default;
                }
            }
            let cursor = kept
                .first()
                .and_then(|c| options.iter().position(|o| o == c))
                .unwrap_or(0);
            Field::Choice {
                cursor,
                chosen: kept,
                options,
                multi,
            }
        }
        (Field::List { cursor, .. }, Field::List { values, .. }) => Field::List {
            cursor: cursor.min(values.len().saturating_sub(1)),
            values,
        },
        (_, fresh) => fresh,
    }
}

fn byte_at(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

/// Line and column of a char index, both counted in chars.
pub fn line_col(s: &str, cursor: usize) -> (usize, usize) {
    s.chars().take(cursor).fold((0, 0), |(line, col), c| {
        if c == '\n' {
            (line + 1, 0)
        } else {
            (line, col + 1)
        }
    })
}

/// Char index of a line and column. The column clamps to the line, and a
/// line past the last one means the end of the text.
fn char_at(s: &str, line: usize, col: usize) -> usize {
    let mut start = 0;
    for (i, l) in s.split('\n').enumerate() {
        let len = l.chars().count();
        if i == line {
            return start + col.min(len);
        }
        start += len + 1;
    }
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use serde_json::json;

    fn spec(v: Value) -> Node {
        volvelle_schema::parse_spec(v).unwrap()
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Event> {
        app.key(KeyEvent::from(code))
    }

    #[test]
    fn form_submits_typed_values() {
        let mut app = App::default();
        app.render(
            "f".into(),
            spec(json!({"type": "box", "children": [
                {"type": "input", "id": "name"},
                {"type": "select", "id": "env", "options": ["dev", "prod"]},
                {"type": "toggle", "id": "force", "label": "Force"},
                {"type": "button", "id": "ok", "label": "OK"}
            ]})),
        );
        for c in "héllo".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Tab);
        let Some(Event::Submit { button, values, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected submit")
        };
        assert_eq!(button, "ok");
        assert_eq!(
            Value::Object(values),
            json!({"name": "héll", "env": "prod", "force": true})
        );
        assert!(app.answered);
        assert!(press(&mut app, KeyCode::Enter).is_none());
    }

    #[test]
    fn rerender_keeps_entered_values() {
        let mut app = App::default();
        let form = json!({"type": "box", "children": [{"type": "input", "id": "q"}]});
        app.render("v".into(), spec(form.clone()));
        press(&mut app, KeyCode::Char('x'));
        app.render("v".into(), spec(form.clone()));
        assert_eq!(app.values()["q"], "x");
        app.render("other".into(), spec(form));
        assert_eq!(app.values()["q"], "");
    }

    #[test]
    fn multiline_edits_across_lines() {
        let mut app = App::default();
        app.render(
            "m".into(),
            spec(json!({"type": "box", "children": [
                {"type": "input", "id": "msg", "multiline": true, "value": "héllo"},
                {"type": "button", "id": "ok", "label": "OK"}
            ]})),
        );
        press(&mut app, KeyCode::Enter);
        for c in "wörld".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.values()["msg"], "héllo\nwörld");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('·'));
        assert_eq!(app.values()["msg"], "hél·lo\nwörld");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.values()["msg"], "hél·lo\nwörd");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert!(matches!(app.fields["msg"], Field::Text { cursor: 0, .. }));
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert!(matches!(app.fields["msg"], Field::Text { cursor: 11, .. }));
        press(&mut app, KeyCode::Tab);
        let Some(Event::Submit { values, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected submit")
        };
        assert_eq!(values["msg"], "hél·lo\nwörd");
    }

    #[test]
    fn locked_view_gives_feedback() {
        let mut app = App::default();
        app.render(
            "l".into(),
            spec(json!({"type": "button", "id": "ok", "label": "OK"})),
        );
        assert!(press(&mut app, KeyCode::Enter).is_some());
        assert!(app.notice.is_none());
        assert!(press(&mut app, KeyCode::Char('x')).is_none());
        let shown = Instant::now();
        assert!(app.notice.is_some());
        assert!(!app.expire(shown));
        assert!(app.expire(shown + NOTICE));
        assert!(app.notice.is_none());
        press(&mut app, KeyCode::PageDown);
        assert!(app.notice.is_none());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }
}
