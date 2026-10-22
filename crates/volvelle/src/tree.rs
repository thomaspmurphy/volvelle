//! The tree atom in the viewer: fold state, keys, and its rows as drawn.

use std::collections::HashMap;

use crossterm::event::KeyCode;
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;
use volvelle_schema::{Flat, TreeNode};

use crate::app::{App, Field};
use crate::draw::palette;

/// A selectable tree's fold state and cursor, indexed in
/// [`TreeNode::flatten`] order.
pub struct TreeState {
    values: Vec<String>,
    parents: Vec<Option<usize>>,
    branches: Vec<bool>,
    open: Vec<bool>,
    /// The node under the cursor, which is always shown.
    cursor: usize,
}

impl TreeState {
    pub fn new(t: &TreeNode) -> Self {
        let flat = t.flatten();
        let mut branches = vec![false; flat.len()];
        for p in flat.iter().filter_map(|f| f.parent) {
            branches[p] = true;
        }
        TreeState {
            values: flat.iter().map(|f| f.value.clone()).collect(),
            parents: flat.iter().map(|f| f.parent).collect(),
            branches,
            open: flat.iter().map(|f| f.item.open).collect(),
            cursor: 0,
        }
    }

    /// Keeps the fold state and cursor of nodes whose values survive a
    /// re-render. A cursor whose node is now hidden moves to its shown
    /// ancestor.
    pub fn carry(mut self, prev: TreeState) -> TreeState {
        let before: HashMap<&str, usize> = prev
            .values
            .iter()
            .enumerate()
            .map(|(i, v)| (v.as_str(), i))
            .collect();
        for (i, v) in self.values.iter().enumerate() {
            if let Some(&j) = before.get(v.as_str()) {
                self.open[i] = prev.open[j];
            }
        }
        let was = prev.values.get(prev.cursor);
        if let Some(i) = self.values.iter().position(|v| Some(v) == was) {
            let shown = shown(&self.parents, &self.open);
            self.cursor = i;
            while !shown[self.cursor] {
                self.cursor = self.parents[self.cursor].unwrap_or(0);
            }
        }
        self
    }

    /// Moves the cursor or folds, returning the node's value on Enter.
    pub fn key(&mut self, code: KeyCode) -> Option<String> {
        let i = self.cursor;
        let shown = shown(&self.parents, &self.open);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(j) = (0..i).rev().find(|&j| shown[j]) {
                    self.cursor = j;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(j) = (i + 1..shown.len()).find(|&j| shown[j]) {
                    self.cursor = j;
                }
            }
            KeyCode::Right | KeyCode::Char('l') if self.branches[i] => {
                if self.open[i] {
                    self.cursor = i + 1;
                } else {
                    self.open[i] = true;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if self.branches[i] && self.open[i] {
                    self.open[i] = false;
                } else if let Some(p) = self.parents[i] {
                    self.cursor = p;
                }
            }
            KeyCode::Char(' ') if self.branches[i] => self.open[i] = !self.open[i],
            KeyCode::Enter => return Some(self.values[i].clone()),
            _ => {}
        }
        None
    }
}

/// The rows a tree shows, in order, and which of them holds the cursor when
/// the tree is selectable. Fold state is the user's once the tree has an id,
/// and the spec's otherwise.
pub fn rows(t: &TreeNode, app: &App) -> (Vec<ListItem<'static>>, Option<usize>) {
    let flat = t.flatten();
    let state = match t.id.as_ref().and_then(|id| app.fields.get(id)) {
        Some(Field::Tree(s)) => Some(s),
        _ => None,
    };
    let parents: Vec<Option<usize>> = flat.iter().map(|f| f.parent).collect();
    let open: Vec<bool> = match state {
        Some(s) => s.open.clone(),
        None => flat.iter().map(|f| f.item.open).collect(),
    };
    let mut items = Vec::new();
    let mut cursor = None;
    for (i, shown) in shown(&parents, &open).into_iter().enumerate() {
        if !shown {
            continue;
        }
        if state.is_some_and(|s| s.cursor == i) {
            cursor = Some(items.len());
        }
        items.push(ListItem::new(line(&flat, i, open[i])));
    }
    (items, cursor)
}

/// Which items are shown: those whose ancestors are all open.
fn shown(parents: &[Option<usize>], open: &[bool]) -> Vec<bool> {
    let mut out: Vec<bool> = Vec::with_capacity(parents.len());
    for p in parents {
        out.push(p.is_none_or(|p| out[p] && open[p]));
    }
    out
}

/// One row: connectors back to the top level, a fold marker on a node with
/// children, the label, and the detail in a muted tone.
fn line(flat: &[Flat], i: usize, open: bool) -> Line<'static> {
    let f = &flat[i];
    let mut guides = Vec::new();
    if f.parent.is_some() {
        guides.push(if f.last { "└── " } else { "├── " });
        let mut up = f.parent;
        while let Some(p) = up.filter(|&p| flat[p].parent.is_some()) {
            guides.push(if flat[p].last { "    " } else { "│   " });
            up = flat[p].parent;
        }
    }
    guides.reverse();
    let mut spans = vec![Span::styled(guides.concat(), palette::muted())];
    if !f.item.children.is_empty() {
        spans.push(Span::raw(if open { "▾ " } else { "▸ " }));
    }
    spans.push(Span::raw(f.item.label.clone()));
    if let Some(d) = &f.item.detail {
        spans.push(Span::styled(format!("  {d}"), palette::muted()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};
    use volvelle_schema::Event;

    use super::*;
    use crate::draw::draw;

    fn app(spec: Value) -> App {
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

    fn files() -> Value {
        json!({"type": "tree", "nodes": [
            {"label": "src", "children": [
                {"label": "app", "children": [
                    {"label": "main.rs", "detail": "4 KB"},
                    {"label": "draw.rs"}
                ]},
                {"label": "gen", "open": false, "children": [{"label": "out.rs"}]},
                {"label": "lib.rs"}
            ]},
            {"label": "Cargo.toml"}
        ]})
    }

    #[test]
    fn draws_connectors_and_fold_markers() {
        let mut term = Terminal::new(TestBackend::new(30, 9)).unwrap();
        let rows = screen(&mut term, &mut app(files()));
        let rows: Vec<&str> = rows.iter().map(|r| r.trim_end()).collect();
        assert_eq!(
            rows[..8],
            [
                "▾ src",
                "├── ▾ app",
                "│   ├── main.rs  4 KB",
                "│   └── draw.rs",
                "├── ▸ gen",
                "└── lib.rs",
                "Cargo.toml",
                "",
            ]
        );
    }

    fn selectable() -> Value {
        let mut spec = files();
        spec["id"] = json!("file");
        spec["title"] = json!("Files");
        spec
    }

    fn press(app: &mut App, keys: &[KeyCode]) -> Option<Event> {
        keys.iter().fold(None, |_, &k| app.key(KeyEvent::from(k)))
    }

    #[test]
    fn keys_fold_unfold_and_answer_with_the_path() {
        use KeyCode::*;
        let mut term = Terminal::new(TestBackend::new(30, 12)).unwrap();
        let mut a = app(selectable());
        let rows = screen(&mut term, &mut a);
        assert!(
            rows[0].contains("Files") && rows[1].contains("› ▾ src"),
            "{rows:#?}"
        );

        // Right on an open node moves to its first child; Left folds it.
        press(&mut a, &[Right, Left]);
        let rows = screen(&mut term, &mut a);
        assert!(rows[2].contains("› ├── ▸ app") && rows[3].contains("├── ▸ gen"));

        // Left on a folded node moves to its parent, Space toggles.
        press(&mut a, &[Left, Char(' ')]);
        let rows = screen(&mut term, &mut a);
        assert!(rows[1].contains("› ▸ src") && rows[2].contains("Cargo.toml"));
        press(&mut a, &[Char(' '), Down, Down]);
        let rows = screen(&mut term, &mut a);
        assert!(rows[3].contains("› ├── ▸ gen"), "{rows:#?}");

        // Right unfolds, then moves in; Enter answers with the label path.
        let Some(Event::Select { id, value, .. }) = press(&mut a, &[Char('l'), Char('l'), Enter])
        else {
            panic!("expected select")
        };
        assert_eq!((id.as_str(), value.as_str()), ("file", "src/gen/out.rs"));
        assert!(a.answered);
    }

    #[test]
    fn rerender_keeps_fold_state_and_cursor() {
        use KeyCode::*;
        let mut a = app(selectable());
        press(&mut a, &[Down, Down, Down, Down, Right, Right]);
        let Some(Event::Select { value, .. }) = press(&mut a, &[Enter]) else {
            panic!("expected select")
        };
        assert_eq!(value, "src/gen/out.rs");

        // The agent adds a file under gen; gen stays open and the cursor stays.
        let mut spec = selectable();
        spec["nodes"][0]["children"][1]["children"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"label": "new.rs"}));
        a.render(
            "t".into(),
            volvelle_schema::parse_spec(spec.clone()).unwrap(),
        );
        let Some(Event::Select { value, .. }) = press(&mut a, &[Enter]) else {
            panic!("expected select")
        };
        assert_eq!(value, "src/gen/out.rs");

        // Folding gen with the cursor inside survives a re-render.
        a.render(
            "t".into(),
            volvelle_schema::parse_spec(spec.clone()).unwrap(),
        );
        press(&mut a, &[Left, Left]);
        a.render(
            "t".into(),
            volvelle_schema::parse_spec(spec.clone()).unwrap(),
        );
        let mut term = Terminal::new(TestBackend::new(30, 12)).unwrap();
        let rows = screen(&mut term, &mut a);
        assert!(rows[5].contains("› ├── ▸ gen"), "{rows:#?}");

        // A new view starts from the spec's fold state.
        a.render("u".into(), volvelle_schema::parse_spec(spec).unwrap());
        let rows = screen(&mut term, &mut a);
        assert!(rows[1].contains("› ▾ src") && rows[5].contains("├── ▸ gen"));
    }

    #[test]
    fn hidden_cursor_moves_to_its_shown_ancestor() {
        let tree = |v: Value| -> TreeNode { serde_json::from_value(v).unwrap() };
        let mut prev = TreeState::new(&tree(json!({"id": "t", "nodes": [
            {"label": "top"}, {"label": "a", "value": "x"}
        ]})));
        prev.key(KeyCode::Down);
        let mut next = TreeState::new(&tree(json!({"id": "t", "nodes": [
            {"label": "top"},
            {"label": "dir", "open": false, "children": [{"label": "a", "value": "x"}]}
        ]})))
        .carry(prev);
        assert_eq!(next.key(KeyCode::Enter).as_deref(), Some("dir"));
    }

    #[test]
    fn tall_tree_scrolls_to_its_cursor() {
        let children: Vec<Value> = (0..30)
            .map(|i| json!({"label": format!("f{i:02}.rs")}))
            .collect();
        let mut a = app(json!({"type": "box", "children": [
            {"type": "tree", "id": "f", "title": "Files", "nodes": [{"label": "src", "children": children}]},
            {"type": "button", "id": "ok", "label": "OK"}
        ]}));
        let mut term = Terminal::new(TestBackend::new(30, 12)).unwrap();
        let rows = screen(&mut term, &mut a);
        assert!(
            rows[10].contains("[ OK ]"),
            "the tree shrinks to fit: {rows:#?}"
        );
        assert!(rows[9].starts_with("╰"));
        for _ in 0..20 {
            a.key(KeyEvent::from(KeyCode::Down));
        }
        let rows = screen(&mut term, &mut a);
        assert!(rows[8].contains("› ├── f19.rs"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("src")));
    }
}
