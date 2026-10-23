//! The graph atom in the viewer: the focus marker, its keys, and the laid
//! out picture as drawn.

pub mod layout;

use crossterm::event::KeyCode;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use volvelle_schema::GraphNode;

use crate::app::{App, Field};
use crate::draw::{palette, tone};
use layout::{Area, Picture};

/// A selectable graph's focus marker and pan, indexed in node order.
pub struct GraphState {
    ids: Vec<String>,
    centers: Vec<(usize, usize)>,
    /// The node under the focus marker.
    cursor: usize,
    /// The first column and row of the picture shown.
    pub pan: (u16, u16),
}

impl GraphState {
    /// The marker starts on the current node, else the first.
    pub fn new(g: &GraphNode) -> Self {
        let p = layout::layout(g);
        let ids: Vec<String> = g.nodes.iter().map(|n| n.id.clone()).collect();
        let cursor = g
            .current
            .as_ref()
            .and_then(|c| ids.iter().position(|id| id == c))
            .unwrap_or(0);
        GraphState {
            centers: p.boxes.iter().map(Area::center).collect(),
            ids,
            cursor,
            pan: (0, 0),
        }
    }

    /// Keeps the marker on its node if the node survives a re-render, and
    /// the pan.
    pub fn carry(mut self, prev: GraphState) -> GraphState {
        let was = prev.ids.get(prev.cursor);
        if let Some(i) = self.ids.iter().position(|id| Some(id) == was) {
            self.cursor = i;
        }
        self.pan = prev.pan;
        self
    }

    /// Moves the marker to the nearest node in an arrow's direction,
    /// returning the node's id on Enter.
    pub fn key(&mut self, code: KeyCode) -> Option<String> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.step(0, -1),
            KeyCode::Down | KeyCode::Char('j') => self.step(0, 1),
            KeyCode::Left | KeyCode::Char('h') => self.step(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.step(1, 0),
            KeyCode::Enter if !self.ids.is_empty() => return Some(self.ids[self.cursor].clone()),
            _ => {}
        }
        None
    }

    /// The nearest node whose centre lies in direction (dx, dy), weighing
    /// distance off that line double. Rows count twice, as a cell is about
    /// twice as tall as it is wide.
    fn step(&mut self, dx: i64, dy: i64) {
        let Some(&(cx, cy)) = self.centers.get(self.cursor) else {
            return;
        };
        let mut best: Option<(i64, usize)> = None;
        for (i, &(x, y)) in self.centers.iter().enumerate() {
            let (ox, oy) = (x as i64 - cx as i64, 2 * (y as i64 - cy as i64));
            let (along, across) = if dx != 0 {
                (ox * dx, oy.abs())
            } else {
                (oy * dy, ox.abs())
            };
            if i == self.cursor || along <= 0 {
                continue;
            }
            let score = along + 2 * across;
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, i));
            }
        }
        if let Some((_, i)) = best {
            self.cursor = i;
        }
    }
}

/// The picture's lines with node borders in their tones, the current node
/// in the accent colour and bold, edge labels muted, and the focus marker
/// on the label of the node under it.
pub fn lines(g: &GraphNode, p: &Picture, app: &App) -> Vec<Line<'static>> {
    let state = match g.id.as_ref().and_then(|id| app.fields.get(id)) {
        Some(Field::Graph(s)) => Some(s),
        _ => None,
    };
    let focused = g.id.as_deref().is_some_and(|id| app.is_focused(id));
    let mut styles = vec![vec![Style::new(); p.width]; p.height];
    let mut paint = |a: &Area, style: Style, border: bool| {
        let (right, bottom) = (a.x + a.width - 1, a.y + a.height - 1);
        for (y, row) in styles.iter_mut().enumerate().skip(a.y).take(a.height) {
            for (x, cell) in row.iter_mut().enumerate().skip(a.x).take(a.width) {
                if !border || y == a.y || y == bottom || x == a.x || x == right {
                    *cell = style;
                }
            }
        }
    };
    for a in &p.notes {
        paint(a, palette::muted(), false);
    }
    for (i, (v, a)) in g.nodes.iter().zip(&p.boxes).enumerate() {
        let current = g.current.as_deref() == Some(v.id.as_str());
        let border = if current {
            palette::accent().add_modifier(Modifier::BOLD)
        } else {
            tone(v.tone)
        };
        paint(a, border, true);
        let label = Area {
            x: a.x + 1,
            y: a.y + 1,
            width: a.width - 2,
            height: a.height - 2,
        };
        let mut text = if current {
            palette::accent().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        if state.is_some_and(|s| s.cursor == i) {
            text = if focused {
                palette::accent().add_modifier(Modifier::REVERSED)
            } else {
                text.add_modifier(Modifier::BOLD)
            };
        }
        paint(&label, text, false);
    }
    p.rows
        .iter()
        .zip(styles)
        .map(|(row, styles)| {
            let mut spans: Vec<Span> = Vec::new();
            let mut run = String::new();
            let mut style = Style::new();
            for (c, s) in row.iter().zip(styles) {
                if s != style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), style));
                }
                style = s;
                run.push(*c);
            }
            spans.push(Span::styled(run, style));
            Line::from(spans)
        })
        .collect()
}

/// The box of the node under the focus marker, which the view pans to keep
/// in sight.
pub fn focus(g: &GraphNode, p: &Picture, app: &App) -> Option<Area> {
    match app.fields.get(g.id.as_ref()?) {
        Some(Field::Graph(s)) => p.boxes.get(s.cursor).copied(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use serde_json::{Value, json};
    use volvelle_schema::{Event, Op};

    use super::*;
    use crate::draw::draw;

    fn app(spec: Value) -> App {
        let mut app = App::default();
        app.render("g".into(), volvelle_schema::parse_spec(spec).unwrap());
        app
    }

    fn screen(term: &mut Terminal<TestBackend>, app: &mut App) -> Vec<String> {
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    fn press(app: &mut App, keys: &[KeyCode]) -> Option<Event> {
        keys.iter().fold(None, |_, &k| app.key(KeyEvent::from(k)))
    }

    /// The style of the first cell showing `text` on screen.
    fn style_of(term: &Terminal<TestBackend>, text: &str) -> Style {
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(i) = row.find(text) {
                let x = row[..i].chars().count() as u16;
                return buf[(x, y)].style();
            }
        }
        panic!("{text:?} is not on screen")
    }

    fn diamond() -> Value {
        json!({"type": "graph", "id": "node", "nodes": [
            {"id": "top"}, {"id": "left"}, {"id": "right", "tone": "error"}, {"id": "bottom"}
        ], "edges": [
            {"from": "top", "to": "left"}, {"from": "top", "to": "right"},
            {"from": "left", "to": "bottom"}, {"from": "right", "to": "bottom"}
        ], "current": "left"})
    }

    #[test]
    fn arrows_move_to_the_nearest_node_and_enter_answers() {
        use KeyCode::{Char, Down, Enter, Left, Right, Up};
        let mut a = app(diamond());
        let mut term = Terminal::new(TestBackend::new(40, 16)).unwrap();
        screen(&mut term, &mut a);
        // The marker starts on the current node.
        assert!(
            style_of(&term, "left")
                .add_modifier
                .contains(Modifier::REVERSED)
        );

        // Nothing lies left of left; right of it is right, then up is top.
        assert!(press(&mut a, &[Left, Right]).is_none());
        screen(&mut term, &mut a);
        assert!(
            style_of(&term, "right")
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !style_of(&term, "left")
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        let Some(Event::Select { id, value, .. }) = press(&mut a, &[Char('k'), Enter]) else {
            panic!("expected select")
        };
        assert_eq!((id.as_str(), value.as_str()), ("node", "top"));
        assert!(a.answered);

        // Down from top reaches a middle node, and down again the bottom.
        let mut a = app(diamond());
        press(&mut a, &[Up, Down, Down]);
        let Some(Event::Select { value, .. }) = press(&mut a, &[Enter]) else {
            panic!("expected select")
        };
        assert_eq!(value, "bottom");
    }

    #[test]
    fn current_is_accented_and_tones_colour_borders() {
        let mut spec = diamond();
        spec.as_object_mut().unwrap().remove("id");
        let mut a = app(spec);
        assert!(
            !a.awaiting_input(),
            "without an id the graph is display only"
        );
        let mut term = Terminal::new(TestBackend::new(40, 16)).unwrap();
        let rows = screen(&mut term, &mut a);
        assert!(rows[0].contains("╭─────╮"), "{rows:#?}");
        let label = style_of(&term, "left");
        assert_eq!(label.fg, Some(Color::Cyan));
        assert!(label.add_modifier.contains(Modifier::BOLD));
        assert!(!label.add_modifier.contains(Modifier::REVERSED));
        // The current node's border, and a toned node's.
        let row = rows.iter().position(|r| r.contains("│ left")).unwrap();
        let col = rows[row].chars().position(|c| c == '│').unwrap() as u16;
        let border = term.backend().buffer()[(col, row as u16)].style();
        assert_eq!(border.fg, Some(Color::Cyan));
        assert!(border.add_modifier.contains(Modifier::BOLD));
        let col = rows[row].find("│ right").unwrap();
        let col = rows[row][..col].chars().count() as u16;
        assert_eq!(
            term.backend().buffer()[(col, row as u16)].style().fg,
            Some(Color::Red)
        );
    }

    #[test]
    fn wide_graph_shrinks_and_pans_to_the_marker() {
        use KeyCode::{Down, Enter, Right};
        let nodes: Vec<Value> = (0..8).map(|i| json!({"id": format!("node{i}")})).collect();
        let edges: Vec<Value> = (1..8)
            .map(|i| json!({"from": "node0", "to": format!("node{i}")}))
            .collect();
        let mut a = app(json!({"type": "box", "children": [
            {"type": "graph", "id": "g", "title": "Fan", "nodes": nodes, "edges": edges},
            {"type": "text", "text": "line\n".repeat(4)},
            {"type": "button", "id": "ok", "label": "OK"}
        ]}));
        let mut term = Terminal::new(TestBackend::new(30, 14)).unwrap();
        let rows = screen(&mut term, &mut a);
        assert!(rows[0].starts_with("╭ Fan"), "{rows:#?}");
        assert!(rows[2].contains("node0"), "{rows:#?}");
        assert!(rows[12].contains("[ OK ]"), "the graph shrinks: {rows:#?}");

        // Down to the children, then right to the last: the view pans.
        press(&mut a, &[Down]);
        for _ in 0..8 {
            press(&mut a, &[Right]);
        }
        let rows = screen(&mut term, &mut a);
        assert!(!rows.iter().any(|r| r.contains("node0")), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("node7")),
            "the marker is in sight: {rows:#?}"
        );
        assert!(!rows.iter().any(|r| r.contains("node1 ")), "{rows:#?}");
        let Some(Field::Graph(g)) = a.fields.get("g") else {
            panic!("expected a graph field")
        };
        assert!(g.pan.0 > 0);
        let Some(Event::Select { value, .. }) = press(&mut a, &[Enter]) else {
            panic!("expected select")
        };
        assert_eq!(value, "node7");
    }

    #[test]
    fn update_moves_the_bound_highlight_and_keeps_the_marker() {
        use KeyCode::{Down, Enter, Right};
        let mut spec = diamond();
        spec.as_object_mut().unwrap().remove("current");
        spec["bind"] = json!("/state");
        let mut a = App::default();
        a.render_with(
            "g".into(),
            volvelle_schema::parse_spec(spec).unwrap(),
            Some(json!({"state": "top"})),
        );
        let mut term = Terminal::new(TestBackend::new(40, 16)).unwrap();
        screen(&mut term, &mut a);
        assert_eq!(style_of(&term, "top").fg, Some(Color::Cyan));
        press(&mut a, &[Down, Right]);

        let patch: Vec<Op> =
            serde_json::from_value(json!([{"op": "replace", "path": "/state", "value": "bottom"}]))
                .unwrap();
        assert!(a.update("g", &patch).is_empty());
        screen(&mut term, &mut a);
        let bottom = style_of(&term, "bottom");
        assert_eq!(bottom.fg, Some(Color::Cyan));
        assert!(bottom.add_modifier.contains(Modifier::BOLD));
        assert_ne!(style_of(&term, "top").fg, Some(Color::Cyan));
        let Some(Event::Select { value, .. }) = press(&mut a, &[Enter]) else {
            panic!("expected select")
        };
        assert_eq!(value, "right", "the marker stayed where the user put it");
    }

    #[test]
    fn rerender_keeps_the_marker_on_its_node() {
        use KeyCode::{Enter, Right};
        let mut a = app(diamond());
        press(&mut a, &[Right]);
        let mut spec = diamond();
        spec["nodes"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({"id": "new"}));
        a.render("g".into(), volvelle_schema::parse_spec(spec).unwrap());
        let Some(Event::Select { value, .. }) = press(&mut a, &[Enter]) else {
            panic!("expected select")
        };
        assert_eq!(value, "right");
    }
}
