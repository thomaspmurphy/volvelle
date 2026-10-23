//! Viewer state: the current spec, what the user has entered, and focus.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Map, Value, json};
use volvelle_schema::library::{self, By, Entry, Library, Origin, Scope};
use volvelle_schema::{Event, Item, Node, Op, Pattern};

use crate::tree::TreeState;

/// How long the status line stays emphasised after a key on a locked view.
const NOTICE: Duration = Duration::from_millis(1500);
/// How long the status line confirms a save.
const SAVED_NOTICE: Duration = Duration::from_secs(3);

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
    /// A choose-with-preview. `scroll` is the preview's first line, back at
    /// the top whenever the cursor moves.
    Chooser {
        cursor: usize,
        values: Vec<String>,
        chosen: Vec<String>,
        multi: bool,
        scroll: usize,
    },
    /// A confirm-destructive button. Confirm stays disabled until the
    /// pattern's typing part holds `require`.
    Confirm {
        pattern: String,
        confirmed: bool,
        require: Option<String>,
    },
    Tree(TreeState),
}

#[derive(Default)]
pub struct App {
    pub view: String,
    /// The spec as drawn, with its binds filled in from `data`.
    pub spec: Option<Node>,
    /// The spec as the agent sent it. Ctrl-S saves this, without data.
    pub template: Option<Node>,
    /// The data document the binds read.
    pub data: Option<Value>,
    /// Bound or not, the fields the user has changed since their value last
    /// came from the spec or data. An update leaves these alone.
    pub edited: HashSet<String>,
    pub fields: HashMap<String, Field>,
    /// Focusable ids in tree order.
    pub order: Vec<String>,
    pub focus: usize,
    /// Set once the user has answered. The view locks until the next render,
    /// so one render gets exactly one answer.
    pub answered: bool,
    /// A transient status message and when it expires.
    pub notice: Option<(Instant, Notice)>,
    /// The Ctrl-S prompt, which takes every key while open.
    pub prompt: Option<Prompt>,
    pub library: Library,
    /// This viewer's herdr pane, recorded as a saved view's origin.
    pub pane: Option<String>,
    pub scroll: Scroll,
    pub quit: bool,
}

pub enum Notice {
    /// A key on a view that has already answered.
    Locked,
    Saved {
        name: String,
        scope: Scope,
    },
    /// Ctrl-S with neither a project nor a user library.
    NoLibrary,
}

/// Asks for a name to save the current view under.
pub struct Prompt {
    pub name: String,
    pub scope: Scope,
    /// Why Enter was refused, shown until the next edit.
    pub error: Option<String>,
    /// Waiting on y or n before replacing an entry with this name.
    pub overwrite: bool,
    /// The view as rendered when Ctrl-S was pressed.
    view: String,
    spec: Node,
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
    /// Renders a spec without data, as most tests do.
    #[cfg(test)]
    pub fn render(&mut self, view: String, spec: Node) {
        self.render_with(view, spec, None);
    }

    /// Shows a spec whose binds read `data`.
    pub fn render_with(&mut self, view: String, spec: Node, data: Option<Value>) {
        if view != self.view {
            self.scroll = Scroll {
                page: self.scroll.page,
                ..Scroll::default()
            };
        }
        self.rebuild(view, spec, data);
        self.answered = false;
        if matches!(self.notice, Some((_, Notice::Locked))) {
            self.notice = None;
        }
    }

    /// Patches the current view's data and redraws it in place. Returns the
    /// pointers the patch touched whose fields the user had edited, which
    /// keep the user's value.
    pub fn update(&mut self, view: &str, patch: &[Op]) -> Vec<String> {
        let (Some(template), Some(data)) = (&self.template, &self.data) else {
            return Vec::new();
        };
        if view != self.view {
            return Vec::new();
        }
        // The server applied the same patch before sending it, so this
        // only fails if the two have drifted; the next render resets both.
        let Ok(data) = volvelle_schema::apply_patch(data, patch) else {
            return Vec::new();
        };
        let mut kept: Vec<String> = Vec::new();
        template.walk(&mut |n| {
            if let (Some(id), Some(at)) = (n.id(), n.bind())
                && self.edited.contains(id)
                && patch
                    .iter()
                    .any(|op| volvelle_schema::touches(op.path(), at))
                && !kept.iter().any(|k| k == at)
            {
                kept.push(at.to_string());
            }
        });
        self.rebuild(self.view.clone(), template.clone(), Some(data));
        kept
    }

    /// Builds fields for a spec. On the same view, focus and what the user
    /// entered carry over, and bound fields they have not edited take their
    /// value from `data`.
    fn rebuild(&mut self, view: String, template: Node, data: Option<Value>) {
        let same_view = view == self.view;
        if !same_view {
            self.edited.clear();
        }
        let spec = match &data {
            Some(data) => volvelle_schema::resolve(&template, data),
            None => template.clone(),
        };
        let mut bound = HashSet::new();
        template.walk(&mut |n| {
            if let (Some(id), Some(_)) = (n.id(), n.bind()) {
                bound.insert(id.to_string());
            }
        });
        let focused = self.focused_id().map(str::to_string);
        let mut old = std::mem::take(&mut self.fields);
        let mut order = Vec::new();
        let mut fields = HashMap::new();
        spec.walk(&mut |node| {
            for (id, fresh) in parts(node) {
                let field = match old.remove(&id) {
                    Some(prev)
                        if same_view && bound.contains(&id) && !self.edited.contains(&id) =>
                    {
                        refresh(prev, fresh)
                    }
                    Some(prev) if same_view => carry(prev, fresh),
                    _ => fresh,
                };
                order.push(id.clone());
                fields.insert(id, field);
            }
        });
        self.edited.retain(|id| fields.contains_key(id));
        self.focus = focused
            .filter(|_| same_view)
            .and_then(|f| order.iter().position(|id| *id == f))
            .or_else(|| start(&spec, &order))
            .unwrap_or(0);
        self.view = view;
        self.order = order;
        self.fields = fields;
        self.spec = Some(spec);
        self.template = Some(template);
        self.data = data;
    }

    pub fn focused_id(&self) -> Option<&str> {
        self.order.get(self.focus).map(String::as_str)
    }

    pub fn is_focused(&self, id: &str) -> bool {
        !self.answered && self.focused_id() == Some(id)
    }

    /// False for a Confirm still waiting on its typed text.
    pub fn enabled(&self, id: &str) -> bool {
        match self.fields.get(id) {
            Some(Field::Confirm {
                pattern,
                confirmed: true,
                require: Some(r),
            }) => matches!(
                self.fields.get(&part(pattern, "typing")),
                Some(Field::Text { value, .. }) if value == r
            ),
            _ => true,
        }
    }

    pub fn focused_field(&self) -> Option<&Field> {
        self.fields
            .get(self.focused_id()?)
            .filter(|_| !self.answered)
    }

    /// Clears an expired notice, returning true when the status line changes.
    pub fn expire(&mut self, now: Instant) -> bool {
        let expired = self.notice.as_ref().is_some_and(|(t, _)| now >= *t);
        if expired {
            self.notice = None;
        }
        expired
    }

    /// True while the view is waiting on the human.
    pub fn awaiting_input(&self) -> bool {
        self.spec.is_some() && !self.answered && !self.order.is_empty()
    }

    /// Field values by id. Pattern parts answer through their pattern.
    pub fn values(&self) -> Map<String, Value> {
        let mut out = Map::new();
        for id in self.order.iter().filter(|id| !id.contains(':')) {
            if let Some(value) = value_of(&self.fields[id]) {
                out.insert(id.clone(), value);
            }
        }
        out
    }

    /// Handles a key press, returning an event when the user answers.
    pub fn key(&mut self, key: KeyEvent) -> Option<Event> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        self.spec.as_ref()?;
        if ctrl && key.code == KeyCode::Char('s') {
            self.open_prompt();
            return None;
        }
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
            self.notice = Some((Instant::now() + NOTICE, Notice::Locked));
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
        let enabled = self.enabled(&id);
        let before = self.fields.get(&id).and_then(value_of);
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
            Field::Tree(tree) => {
                if let Some(value) = tree.key(key.code) {
                    return Some(self.answer(|view, values| Event::Select {
                        view,
                        id,
                        value,
                        values,
                    }));
                }
            }
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
            Field::Chooser {
                cursor,
                values,
                chosen,
                multi,
                scroll,
            } => {
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let was = *cursor;
                match key.code {
                    KeyCode::Up if shift => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down if shift => *scroll += 1,
                    KeyCode::Char('K') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Char('J') => *scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        *cursor = (*cursor + 1).min(values.len().saturating_sub(1))
                    }
                    KeyCode::Char(' ') if *multi => {
                        let v = &values[*cursor];
                        if let Some(i) = chosen.iter().position(|c| c == v) {
                            chosen.remove(i);
                        } else {
                            chosen.push(v.clone());
                        }
                    }
                    KeyCode::Enter if !values.is_empty() => {
                        // With nothing toggled, Enter takes the item under the cursor.
                        let selected = if !*multi {
                            Value::String(values[*cursor].clone())
                        } else if chosen.is_empty() {
                            json!([values[*cursor]])
                        } else {
                            values
                                .iter()
                                .filter(|v| chosen.contains(v))
                                .cloned()
                                .collect()
                        };
                        let value = json!({"selected": selected});
                        return Some(self.answer(|view, values| Event::Answer {
                            view,
                            id,
                            value,
                            values,
                        }));
                    }
                    _ => {}
                }
                if *cursor != was {
                    *scroll = 0;
                }
            }
            Field::Confirm {
                pattern, confirmed, ..
            } => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') if enabled => {
                    let (id, value) = (pattern.clone(), json!({"confirmed": *confirmed}));
                    return Some(self.answer(|view, values| Event::Answer {
                        view,
                        id,
                        value,
                        values,
                    }));
                }
                // No letter keys: a stray keystroke must not reach Confirm.
                KeyCode::Left | KeyCode::Up => return self.move_focus(-1),
                KeyCode::Right | KeyCode::Down => return self.move_focus(1),
                _ => {}
            },
        }
        if self.fields.get(&id).and_then(value_of) != before {
            self.edited.insert(id);
        }
        None
    }

    /// Opens the save prompt on the view as it is now, answered or not.
    fn open_prompt(&mut self) {
        let Some(spec) = &self.template else { return };
        let Some(&scope) = self.library.scopes().first() else {
            self.notice = Some((Instant::now() + SAVED_NOTICE, Notice::NoLibrary));
            return;
        };
        self.prompt = Some(Prompt {
            name: String::new(),
            scope,
            error: None,
            overwrite: false,
            view: self.view.clone(),
            spec: spec.clone(),
        });
    }

    /// Keys while the prompt is open. None of them reach the view.
    fn prompt_key(&mut self, key: KeyEvent) -> Option<Event> {
        let scopes = self.library.scopes();
        let p = self.prompt.as_mut()?;
        if p.overwrite {
            match key.code {
                KeyCode::Char('y' | 'Y') => return self.save(),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => p.overwrite = false,
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Tab | KeyCode::BackTab => {
                let i = scopes.iter().position(|s| *s == p.scope).unwrap_or(0);
                p.scope = scopes[(i + 1) % scopes.len()];
            }
            KeyCode::Enter => {
                if p.name.is_empty() {
                    p.error = Some("type a name".into());
                } else if library::valid_name(&p.name).is_err() {
                    p.error = Some("use a-z, 0-9 and -".into());
                } else if self.library.exists(p.scope, &p.name) {
                    p.overwrite = true;
                } else {
                    return self.save();
                }
            }
            KeyCode::Backspace => {
                p.name.pop();
                p.error = None;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                p.name.push(c);
                p.error = None;
            }
            _ => {}
        }
        None
    }

    /// Writes the prompt's entry and closes the prompt, or shows why not.
    fn save(&mut self) -> Option<Event> {
        let p = self.prompt.as_mut()?;
        let origin = Origin {
            view: p.view.clone(),
            pane: self.pane.clone(),
            by: By::User,
        };
        let entry = Entry::new(&p.name, &p.spec, origin);
        match self.library.save(p.scope, &entry) {
            Ok(path) => {
                let Prompt {
                    name, scope, view, ..
                } = self.prompt.take()?;
                let saved = Notice::Saved {
                    name: name.clone(),
                    scope,
                };
                self.notice = Some((Instant::now() + SAVED_NOTICE, saved));
                Some(Event::Saved {
                    view,
                    name,
                    scope,
                    path: path.display().to_string(),
                })
            }
            Err(e) => {
                p.overwrite = false;
                p.error = Some(e.to_string());
                None
            }
        }
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

/// The id of a pattern's part, such as `drop:cancel`. Spec ids may not
/// contain `:`, so these never clash.
pub fn part(pattern: &str, name: &str) -> String {
    format!("{pattern}:{name}")
}

/// The focusable fields a node adds, in focus order.
fn parts(node: &Node) -> Vec<(String, Field)> {
    match node {
        Node::Pattern(Pattern::ConfirmDestructive(c)) => {
            let button = |confirmed| Field::Confirm {
                pattern: c.id.clone(),
                confirmed,
                require: c.require_typing.clone(),
            };
            let mut out = Vec::new();
            if c.require_typing.is_some() {
                let typing = Field::Text {
                    value: String::new(),
                    cursor: 0,
                    top: 0,
                    multiline: false,
                };
                out.push((part(&c.id, "typing"), typing));
            }
            out.push((part(&c.id, "cancel"), button(false)));
            out.push((part(&c.id, "confirm"), button(true)));
            out
        }
        Node::Pattern(Pattern::ChooseWithPreview(c)) => {
            let chooser = Field::Chooser {
                cursor: 0,
                values: c.items.iter().map(|i| i.value().to_string()).collect(),
                chosen: Vec::new(),
                multi: c.multi,
                scroll: 0,
            };
            vec![(c.id.clone(), chooser)]
        }
        n => n
            .id()
            .map(|id| (id.to_string(), init(n)))
            .into_iter()
            .collect(),
    }
}

/// Where focus starts in a fresh view: on Cancel if the view asks to
/// confirm something destructive, else on the first field.
fn start(spec: &Node, order: &[String]) -> Option<usize> {
    let mut cancel = None;
    spec.walk(&mut |n| {
        if let Node::Pattern(Pattern::ConfirmDestructive(c)) = n {
            cancel = Some(part(&c.id, "cancel"));
        }
    });
    order.iter().position(|id| Some(id) == cancel.as_ref())
}

fn init(node: &Node) -> Field {
    match node {
        Node::Input(n) => Field::Text {
            cursor: n.value().chars().count(),
            value: n.value().to_string(),
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
        Node::Toggle(n) => Field::Toggle(n.value()),
        Node::Tree(n) => Field::Tree(TreeState::new(n)),
        Node::List(n) => Field::List {
            cursor: 0,
            values: n
                .items()
                .iter()
                .map(|i| Item::value(i).to_string())
                .collect(),
        },
        _ => Field::Button,
    }
}

/// What a field contributes to `values`, if anything.
fn value_of(field: &Field) -> Option<Value> {
    Some(match field {
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
        Field::List { .. }
        | Field::Button
        | Field::Chooser { .. }
        | Field::Confirm { .. }
        | Field::Tree(_) => return None,
    })
}

/// A bound field the user has not edited takes its new value, keeping its
/// cursor where it can. A cursor at the end stays at the end.
fn refresh(prev: Field, fresh: Field) -> Field {
    match (prev, fresh) {
        (
            Field::Text {
                value: old,
                cursor,
                top,
                ..
            },
            Field::Text {
                value, multiline, ..
            },
        ) => {
            let len = value.chars().count();
            let cursor = if cursor >= old.chars().count() {
                len
            } else {
                cursor.min(len)
            };
            Field::Text {
                value,
                cursor,
                top,
                multiline,
            }
        }
        (prev @ Field::List { .. }, fresh @ Field::List { .. }) => carry(prev, fresh),
        (_, fresh) => fresh,
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
        (
            Field::Chooser {
                cursor,
                chosen,
                scroll,
                ..
            },
            Field::Chooser { values, multi, .. },
        ) => Field::Chooser {
            cursor: cursor.min(values.len().saturating_sub(1)),
            chosen: chosen.into_iter().filter(|c| values.contains(c)).collect(),
            values,
            multi,
            scroll,
        },
        (Field::List { cursor, .. }, Field::List { values, .. }) => Field::List {
            cursor: cursor.min(values.len().saturating_sub(1)),
            values,
        },
        (Field::Tree(prev), Field::Tree(fresh)) => Field::Tree(fresh.carry(prev)),
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

    fn confirm(require: Option<&str>) -> Node {
        spec(json!({"type": "box", "children": [
            {"type": "text", "text": "Branch cleanup"},
            {"type": "pattern", "name": "confirm-destructive@1", "id": "drop",
             "summary": "Delete `old`", "consequence": "Unmerged commits are lost",
             "require_typing": require}
        ]}))
    }

    #[test]
    fn confirm_starts_on_cancel_and_answers_its_value() {
        let mut app = App::default();
        app.render("c".into(), confirm(None));
        assert_eq!(app.focused_id(), Some("drop:cancel"));
        let Some(Event::Answer { id, value, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!((id.as_str(), value), ("drop", json!({"confirmed": false})));

        app.render("c2".into(), confirm(None));
        press(&mut app, KeyCode::Right);
        let Some(Event::Answer { value, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!(value, json!({"confirmed": true}));

        app.render("c3".into(), confirm(None));
        let Some(Event::Cancel { values, .. }) = press(&mut app, KeyCode::Esc) else {
            panic!("Esc cancels the view")
        };
        assert!(values.is_empty());
    }

    #[test]
    fn confirm_ignores_letter_keys_for_focus() {
        let mut app = App::default();
        app.render("c".into(), confirm(None));
        for c in "hjkl".chars() {
            assert!(press(&mut app, KeyCode::Char(c)).is_none());
            assert_eq!(app.focused_id(), Some("drop:cancel"));
        }
        press(&mut app, KeyCode::Right);
        assert_eq!(app.focused_id(), Some("drop:confirm"));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.focused_id(), Some("drop:confirm"));
        press(&mut app, KeyCode::Left);
        assert_eq!(app.focused_id(), Some("drop:cancel"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focused_id(), Some("drop:confirm"));
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focused_id(), Some("drop:cancel"));
    }

    #[test]
    fn confirm_waits_for_the_typed_text() {
        let mut app = App::default();
        app.render("c".into(), confirm(Some("old")));
        assert_eq!(app.order, ["drop:typing", "drop:cancel", "drop:confirm"]);
        assert_eq!(app.focused_id(), Some("drop:cancel"));
        press(&mut app, KeyCode::Tab);
        assert!(!app.enabled("drop:confirm"));
        assert!(press(&mut app, KeyCode::Enter).is_none(), "disabled");
        press(&mut app, KeyCode::Tab);
        for c in "ol".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        app.render("c".into(), confirm(Some("old")));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.enabled("drop:confirm"), "typing survives a re-render");
        assert!(app.values().is_empty(), "parts are not field values");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focused_id(), Some("drop:confirm"));
        let Some(Event::Answer { value, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!(value, json!({"confirmed": true}));
    }

    fn chooser(multi: bool) -> Node {
        let items: Vec<Value> = ["a", "b", "c"]
            .iter()
            .map(|v| json!({"label": v.to_uppercase(), "value": v, "preview": format!("# {v}")}))
            .collect();
        spec(
            json!({"type": "pattern", "name": "choose-with-preview@1", "id": "pick",
                    "items": items, "multi": multi}),
        )
    }

    #[test]
    fn chooser_answers_the_item_under_the_cursor() {
        let mut app = App::default();
        app.render("c".into(), chooser(false));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('J'));
        assert!(matches!(
            app.fields["pick"],
            Field::Chooser { scroll: 1, .. }
        ));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
        assert!(matches!(
            app.fields["pick"],
            Field::Chooser {
                cursor: 1,
                scroll: 0,
                ..
            }
        ));
        let Some(Event::Answer { id, value, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!((id.as_str(), value), ("pick", json!({"selected": "b"})));
    }

    #[test]
    fn chooser_answer_carries_field_values() {
        let mut app = App::default();
        app.render(
            "f".into(),
            spec(json!({"type": "box", "children": [
                {"type": "input", "id": "note"},
                {"type": "pattern", "name": "choose-with-preview@1", "id": "pick",
                 "items": [{"label": "a", "preview": "A"}]}
            ]})),
        );
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Tab);
        let Some(Event::Answer { value, values, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!(value, json!({"selected": "a"}));
        assert_eq!(Value::Object(values), json!({"note": "x"}));
    }

    #[test]
    fn multi_chooser_toggles_with_space() {
        let mut app = App::default();
        app.render("m".into(), chooser(true));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        app.render("m".into(), chooser(true));
        let Some(Event::Answer { value, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!(value, json!({"selected": ["a", "c"]}), "in item order");

        app.render("m2".into(), chooser(true));
        press(&mut app, KeyCode::Down);
        let Some(Event::Answer { value, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected an answer")
        };
        assert_eq!(value, json!({"selected": ["b"]}));
    }

    fn libraries(tag: &str) -> Library {
        let root = std::env::temp_dir().join(format!(
            "volvelle-app-{tag}-{}-{}",
            std::process::id(),
            library::now_ms()
        ));
        Library {
            project: Some(root.join("project")),
            user: Some(root.join("user")),
        }
    }

    fn cleanup(lib: Library) {
        let root = lib.user.unwrap().parent().unwrap().to_path_buf();
        let _ = std::fs::remove_dir_all(root);
    }

    fn ctrl(app: &mut App, c: char) -> Option<Event> {
        app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            assert!(press(app, KeyCode::Char(c)).is_none());
        }
    }

    fn sheet() -> Node {
        spec(json!({"type": "box", "title": "Sheet", "children": [
            {"type": "input", "id": "hero", "value": "Ada"},
            {"type": "button", "id": "ok", "label": "OK"}
        ]}))
    }

    #[test]
    fn ctrl_s_saves_the_spec_as_rendered() {
        let lib = libraries("save");
        let mut app = App {
            library: lib.clone(),
            pane: Some("p9".into()),
            ..App::default()
        };
        app.render("sheet".into(), sheet());
        type_str(&mut app, "!");
        assert!(ctrl(&mut app, 's').is_none());
        let p = app.prompt.as_ref().unwrap();
        assert_eq!((p.name.as_str(), p.scope), ("", Scope::Project));

        type_str(&mut app, "My Hero");
        assert_eq!(app.prompt.as_ref().unwrap().name, "My Hero");
        assert_eq!(app.values()["hero"], "Ada!", "typing goes to the prompt");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert!(app.prompt.as_ref().unwrap().error.is_some());
        for _ in 0.."My Hero".len() {
            press(&mut app, KeyCode::Backspace);
        }
        assert!(app.prompt.as_ref().unwrap().error.is_none());
        type_str(&mut app, "hero");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.prompt.as_ref().unwrap().scope, Scope::User);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.prompt.as_ref().unwrap().scope, Scope::Project);
        press(&mut app, KeyCode::Tab);

        let Some(Event::Saved {
            view,
            name,
            scope,
            path,
        }) = press(&mut app, KeyCode::Enter)
        else {
            panic!("expected a save")
        };
        assert_eq!(
            (view.as_str(), name.as_str(), scope),
            ("sheet", "hero", Scope::User)
        );
        assert!(app.prompt.is_none());
        assert!(matches!(app.notice, Some((_, Notice::Saved { .. }))));
        assert!(!app.answered, "saving is not an answer");
        assert_eq!(app.values()["hero"], "Ada!");
        let (_, at, entry) = lib.load("hero").unwrap();
        assert_eq!(at.display().to_string(), path);
        assert_eq!(entry.spec["children"][0]["value"], "Ada");
        assert_eq!(entry.origin.by, By::User);
        assert_eq!(
            (entry.origin.view.as_str(), entry.origin.pane.as_deref()),
            ("sheet", Some("p9"))
        );
        assert!(!lib.exists(Scope::Project, "hero"));
        cleanup(lib);
    }

    #[test]
    fn ctrl_s_asks_before_overwriting() {
        let lib = libraries("overwrite");
        let mut app = App {
            library: lib.clone(),
            ..App::default()
        };
        app.render("sheet".into(), sheet());
        ctrl(&mut app, 's');
        type_str(&mut app, "map");
        assert!(press(&mut app, KeyCode::Enter).is_some());

        ctrl(&mut app, 's');
        type_str(&mut app, "map");
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert!(app.prompt.as_ref().unwrap().overwrite);
        assert!(press(&mut app, KeyCode::Char('x')).is_none());
        press(&mut app, KeyCode::Char('n'));
        let p = app.prompt.as_ref().unwrap();
        assert!(!p.overwrite && p.name == "map");
        press(&mut app, KeyCode::Enter);
        let Some(Event::Saved { scope, .. }) = press(&mut app, KeyCode::Char('y')) else {
            panic!("expected a save")
        };
        assert_eq!(scope, Scope::Project);
        assert!(app.prompt.is_none());
        cleanup(lib);
    }

    #[test]
    fn esc_closes_the_prompt_without_cancelling_the_view() {
        let mut app = App {
            library: Library {
                project: None,
                user: Some(std::env::temp_dir().join("volvelle-app-unused")),
            },
            ..App::default()
        };
        app.render("sheet".into(), sheet());
        ctrl(&mut app, 's');
        assert_eq!(app.prompt.as_ref().unwrap().scope, Scope::User);
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.prompt.as_ref().unwrap().scope,
            Scope::User,
            "no project"
        );
        type_str(&mut app, "q");
        assert!(press(&mut app, KeyCode::Esc).is_none());
        assert!(app.prompt.is_none() && !app.answered);
        assert_eq!(app.values()["hero"], "Ada");

        press(&mut app, KeyCode::Tab);
        assert!(press(&mut app, KeyCode::Enter).is_some());
        ctrl(&mut app, 's');
        assert!(app.prompt.is_some(), "an answered view can be saved");
        assert!(app.notice.is_none());

        let mut bare = App::default();
        bare.render("sheet".into(), sheet());
        ctrl(&mut bare, 's');
        assert!(bare.prompt.is_none());
        assert!(matches!(bare.notice, Some((_, Notice::NoLibrary))));
    }

    fn bound() -> (Node, Value) {
        let spec = spec(json!({"type": "box", "children": [
            {"type": "progress", "label": "HP", "bind": "/hp"},
            {"type": "input", "id": "name", "bind": "/name"},
            {"type": "toggle", "id": "poisoned", "label": "Poisoned", "bind": "/poisoned"},
            {"type": "list", "id": "bag", "bind": "/bag"},
            {"type": "input", "id": "note"},
            {"type": "button", "id": "ok", "label": "OK"}
        ]}));
        let data = json!({"hp": 0.5, "name": "Ada", "poisoned": false, "bag": ["rope", "map"]});
        (spec, data)
    }

    fn ops(v: Value) -> Vec<Op> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn binds_fill_fields_and_drawn_atoms() {
        let (spec, data) = bound();
        let mut app = App::default();
        app.render_with("s".into(), spec, Some(data));
        assert_eq!(
            Value::Object(app.values()),
            json!({"name": "Ada", "poisoned": false, "note": ""})
        );
        let Some(Node::Box(b)) = &app.spec else {
            panic!()
        };
        let Node::Progress(p) = &b.children[0] else {
            panic!()
        };
        assert_eq!(p.value(), 0.5);
        assert!(matches!(&app.fields["bag"], Field::List { values, .. } if values.len() == 2));
    }

    #[test]
    fn updates_redraw_in_place_and_user_edits_win() {
        let (spec, data) = bound();
        let mut app = App::default();
        app.render_with("s".into(), spec, Some(data));
        type_str(&mut app, "!");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        type_str(&mut app, "hi");
        app.scroll.offset = 2;
        assert_eq!(app.edited, HashSet::from(["name".into(), "note".into()]));

        let patch = ops(json!([
            {"op": "replace", "path": "/hp", "value": 0.25},
            {"op": "replace", "path": "/name", "value": "Grace"},
            {"op": "replace", "path": "/poisoned", "value": true},
            {"op": "remove", "path": "/bag/0"}
        ]));
        assert_eq!(app.update("s", &patch), ["/name"]);
        assert_eq!(
            Value::Object(app.values()),
            json!({"name": "Ada!", "poisoned": true, "note": "hi"})
        );
        assert_eq!(app.focused_id(), Some("note"));
        assert_eq!(app.scroll.offset, 2);
        assert!(matches!(
            &app.fields["bag"],
            Field::List { cursor: 0, values } if values == &["map"]
        ));
        assert_eq!(app.data.as_ref().unwrap()["hp"], 0.25);
        let Some(Node::Box(b)) = &app.spec else {
            panic!()
        };
        let Node::Progress(p) = &b.children[0] else {
            panic!()
        };
        assert_eq!(p.value(), 0.25);

        // Still the user's: a later update touching it keeps it again.
        let whole = ops(json!([{"op": "replace", "path": "", "value":
            {"hp": 1.0, "name": "Lin", "poisoned": false, "bag": []}}]));
        assert_eq!(app.update("s", &whole), ["/name"]);
        assert_eq!(app.values()["poisoned"], false);
        assert!(app.update("other", &patch).is_empty(), "another view");
        assert!(!app.answered);
    }

    #[test]
    fn ctrl_s_saves_a_bound_view_without_its_data() {
        let lib = libraries("bound");
        let mut app = App {
            library: lib.clone(),
            ..App::default()
        };
        let (spec, data) = bound();
        app.render_with("s".into(), spec, Some(data));
        ctrl(&mut app, 's');
        type_str(&mut app, "sheet");
        assert!(press(&mut app, KeyCode::Enter).is_some());
        let (_, _, entry) = lib.load("sheet").unwrap();
        assert_eq!(entry.spec["children"][1]["bind"], "/name");
        assert!(entry.spec["children"][1].get("value").is_none());
        assert!(entry.spec["children"][0].get("value").is_none());
        cleanup(lib);
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
        assert!(matches!(app.notice, Some((_, Notice::Locked))));
        assert!(!app.expire(shown));
        assert!(app.expire(shown + NOTICE));
        assert!(app.notice.is_none());
        press(&mut app, KeyCode::PageDown);
        assert!(app.notice.is_none());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }
}
