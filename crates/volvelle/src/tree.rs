//! The tree atom in the viewer: its rows as drawn.

use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;
use volvelle_schema::{Flat, TreeNode};

use crate::draw::palette;

/// The rows a tree shows, in order.
pub fn rows(t: &TreeNode) -> Vec<ListItem<'static>> {
    let flat = t.flatten();
    let parents: Vec<Option<usize>> = flat.iter().map(|f| f.parent).collect();
    let open: Vec<bool> = flat.iter().map(|f| f.item.open).collect();
    shown(&parents, &open)
        .into_iter()
        .enumerate()
        .filter(|&(_, s)| s)
        .map(|(i, _)| ListItem::new(line(&flat, i, open[i])))
        .collect()
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};

    use crate::app::App;
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
}
