//! Graph layout with no terminal types: a graph in, a grid of characters and
//! each node's box out. Layers in the Sugiyama style: back-edges found by a
//! depth-first search are reversed, nodes are layered by longest path, edges
//! that span layers get dummy nodes, a few barycenter sweeps order each
//! layer, and edges are routed with box-drawing lines through the gaps
//! between layers. Everything is computed along two axes, rank (across
//! layers) and order (within a layer), so `right` is the same layout
//! transposed. The same graph always gives the same picture.

use std::collections::HashMap;

use volvelle_schema::{Flow, GraphNode};

/// Columns between neighbours in a layer.
const SPACING: usize = 2;
/// Barycenter sweeps when ordering layers, alternately down and up.
const SWEEPS: usize = 8;

/// A laid-out graph.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    /// `height` rows of `width` characters, blank cells as spaces.
    pub rows: Vec<Vec<char>>,
    /// Each node's box, in the graph's node order.
    pub boxes: Vec<Area>,
    /// Edge labels and self-loop markers, as runs of cells.
    pub notes: Vec<Area>,
}

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Area {
    /// The cell at the centre, where a node's label sits.
    pub fn center(&self) -> (usize, usize) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

impl Picture {
    /// The picture as text, without trailing spaces.
    #[cfg(test)]
    pub fn text(&self) -> String {
        let lines: Vec<String> = self
            .rows
            .iter()
            .map(|r| r.iter().collect::<String>().trim_end().to_string())
            .collect();
        lines.join("\n")
    }
}

/// What occupies a slot in a layer.
enum Kind {
    Node,
    /// A point on an edge that spans layers. Dummies of reversed edges keep
    /// to the side of their layer.
    Dummy {
        back: bool,
        label: Option<String>,
    },
}

struct Item {
    kind: Kind,
    layer: usize,
    /// Footprint along the order axis, and thickness along the rank axis.
    size_o: usize,
    size_r: usize,
    /// For a node, its box; a dummy has none.
    box_o: usize,
    box_r: usize,
    /// Offset of the item's centre along the order axis. A dummy's line
    /// runs through it.
    mid: usize,
    /// Position of the footprint along the order axis.
    pos: usize,
}

/// One step of an edge, between an item in one layer and an item in the
/// next. `upper` is in the earlier layer.
struct Segment {
    edge: usize,
    upper: usize,
    lower: usize,
    /// Where it leaves `upper` and enters `lower`, as offsets into each
    /// item and then, once items are placed, as order-axis positions.
    a: usize,
    b: usize,
    /// Track in the gap for a bent segment, counted from the upper layer.
    track: usize,
    /// For a segment that doglegs, the free column it drops through and
    /// the track of its second run.
    dogleg: Option<(usize, usize)>,
}

/// A non-self edge, oriented so it runs from an earlier layer to a later
/// one. A reversed edge's arrow points back up, at `top`.
struct Route {
    top: usize,
    bottom: usize,
    back: bool,
}

pub fn layout(g: &GraphNode) -> Picture {
    let n = g.nodes.len();
    let index: HashMap<&str, usize> = g
        .nodes
        .iter()
        .enumerate()
        .map(|(i, v)| (v.id.as_str(), i))
        .collect();
    let mut edges: Vec<(usize, usize, Option<&str>)> = Vec::new();
    let mut loops: Vec<Vec<&str>> = vec![Vec::new(); n];
    let mut looped = vec![false; n];
    for e in &g.edges {
        let (Some(&u), Some(&v)) = (index.get(e.from.as_str()), index.get(e.to.as_str())) else {
            continue;
        };
        if u == v {
            looped[u] = true;
            loops[u].extend(e.label.as_deref());
        } else {
            edges.push((u, v, e.label.as_deref()));
        }
    }
    let back = back_edges(n, &edges);
    let routes: Vec<Route> = edges
        .iter()
        .zip(&back)
        .map(|(&(u, v, _), &back)| {
            let (top, bottom) = if back { (v, u) } else { (u, v) };
            Route { top, bottom, back }
        })
        .collect();
    let mut layer = layers(n, &routes);
    // A labelled edge needs a dummy to hold its label, so every edge then
    // spans at least two layers.
    if edges.iter().any(|e| e.2.is_some()) {
        layer.iter_mut().for_each(|l| *l *= 2);
    }

    let flow = g.direction;
    let mut items: Vec<Item> = (0..n)
        .map(|i| Item {
            kind: Kind::Node,
            layer: layer[i],
            size_o: 0,
            size_r: 0,
            box_o: 0,
            box_r: 0,
            mid: 0,
            pos: 0,
        })
        .collect();
    let mut segments = Vec::new();
    for (e, r) in routes.iter().enumerate() {
        let (from, to) = (layer[r.top], layer[r.bottom]);
        let mid = from + 1 + (to - from).saturating_sub(2) / 2;
        let mut prev = r.top;
        for l in from + 1..to {
            let label = edges[e].2.filter(|_| l == mid).map(str::to_string);
            let (size_o, size_r, at) = match (&label, flow) {
                (None, _) => (1, 1, 0),
                (Some(t), Flow::Down) => (2 + width(t), 1, 0),
                (Some(t), Flow::Right) => (2, width(t), 1),
            };
            items.push(Item {
                kind: Kind::Dummy {
                    back: r.back,
                    label,
                },
                layer: l,
                size_o,
                size_r,
                box_o: 0,
                box_r: 0,
                mid: at,
                pos: 0,
            });
            let d = items.len() - 1;
            segments.push(segment(e, prev, d));
            prev = d;
        }
        segments.push(segment(e, prev, r.bottom));
    }

    // Node sizes depend on how many edges meet each side.
    let mut above = vec![0; items.len()];
    let mut below = vec![0; items.len()];
    for s in &segments {
        below[s.upper] += 1;
        above[s.lower] += 1;
    }
    let markers: Vec<Option<String>> = (0..n)
        .map(|i| {
            looped[i].then(|| match loops[i].is_empty() {
                true => "↺".to_string(),
                false => format!("↺ {}", loops[i].join(", ")),
            })
        })
        .collect();
    for i in 0..n {
        let label = width(g.nodes[i].label());
        let ports = above[i].max(below[i]);
        let marker = markers[i].as_deref().map_or(0, width);
        let it = &mut items[i];
        match flow {
            Flow::Down => {
                it.box_o = (label + 4).max(2 * ports + 1);
                it.box_r = 3;
                it.size_o = it.box_o + if marker > 0 { 1 + marker } else { 0 };
                it.size_r = 3;
            }
            Flow::Right => {
                it.box_o = 3.max(2 * ports + 1);
                it.box_r = label + 4;
                it.size_o = it.box_o + usize::from(marker > 0);
                it.size_r = it.box_r.max(marker);
            }
        }
        it.mid = it.box_o / 2;
    }

    let depth = items.iter().map(|it| it.layer + 1).max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for (i, it) in items.iter().enumerate() {
        layers[it.layer].push(i);
    }
    order(&mut layers, &items, &segments);
    ports(&items, &layers, &mut segments);
    place(&mut items, &layers, &segments);
    for s in &mut segments {
        s.a += items[s.upper].pos;
        s.b += items[s.lower].pos;
    }
    let gaps = tracks(&items, &layers, &mut segments, &routes);

    // Bands along the rank axis: each layer, then the gap after it.
    let mut start = Vec::with_capacity(depth);
    let mut r = 0;
    for (l, ids) in layers.iter().enumerate() {
        start.push(r);
        r += ids.iter().map(|&i| items[i].size_r).max().unwrap_or(0);
        if l + 1 < depth {
            r += gaps[l].rows;
        }
    }
    let span_o = items.iter().map(|it| it.pos + it.size_o).max().unwrap_or(0);
    let mut grid = Grid::new(flow, span_o, r);
    let thick = |l: usize| -> usize {
        layers[l]
            .iter()
            .map(|&i| items[i].size_r)
            .max()
            .unwrap_or(0)
    };

    for s in &segments {
        let (up, low) = (&items[s.upper], &items[s.lower]);
        let route = &routes[s.edge];
        let arrow_up = route.back && matches!(up.kind, Kind::Node);
        let arrow_down = !route.back && matches!(low.kind, Kind::Node);
        let gap = &gaps[up.layer];
        let g0 = start[up.layer] + thick(up.layer);
        let from = match up.kind {
            Kind::Node => start[up.layer] + up.box_r - 1 + usize::from(arrow_up),
            Kind::Dummy { .. } => start[up.layer],
        };
        let to = match low.kind {
            Kind::Node => start[low.layer] - usize::from(arrow_down),
            Kind::Dummy { .. } => start[low.layer],
        };
        let t = g0 + gap.top + s.track;
        match s.dogleg {
            _ if s.a == s.b => grid.line(&[(s.a, from), (s.a, to)]),
            None => grid.line(&[(s.a, from), (s.a, t), (s.b, t), (s.b, to)]),
            Some((c, t2)) => {
                let t2 = g0 + gap.top + t2;
                grid.line(&[(s.a, from), (s.a, t), (c, t), (c, t2), (s.b, t2), (s.b, to)]);
            }
        }
        if arrow_up {
            grid.put(s.a, from, grid.arrow(false));
        }
        if arrow_down {
            grid.put(s.b, to, grid.arrow(true));
        }
    }

    let mut boxes = Vec::with_capacity(n);
    let mut notes = Vec::new();
    for (i, v) in g.nodes.iter().enumerate() {
        let it = &items[i];
        let r = start[it.layer];
        let area = grid.area(it.pos, r, it.box_o, it.box_r);
        grid.frame(area);
        let label = v.label();
        let (x, y) = match flow {
            Flow::Down => (area.x + 1 + (area.width - 2 - width(label)) / 2, area.y + 1),
            Flow::Right => (area.x + 2, area.y + area.height / 2),
        };
        grid.text(x, y, label);
        if let Some(m) = &markers[i] {
            let (x, y) = match flow {
                Flow::Down => (area.x + area.width + 1, area.y + 1),
                Flow::Right => (area.x, area.y + area.height),
            };
            grid.text(x, y, m);
            notes.push(Area {
                x,
                y,
                width: width(m),
                height: 1,
            });
        }
        boxes.push(area);
    }
    for it in &items {
        let Kind::Dummy { label: Some(t), .. } = &it.kind else {
            continue;
        };
        let r = start[it.layer];
        let (x, y) = match flow {
            Flow::Down => (it.pos + 2, r),
            Flow::Right => (r, it.pos),
        };
        grid.text(x, y, t);
        notes.push(Area {
            x,
            y,
            width: width(t),
            height: 1,
        });
    }
    grid.finish(boxes, notes)
}

fn segment(edge: usize, upper: usize, lower: usize) -> Segment {
    Segment {
        edge,
        upper,
        lower,
        a: 0,
        b: 0,
        track: 0,
        dogleg: None,
    }
}

fn width(s: &str) -> usize {
    s.chars().count()
}

/// Which edges a depth-first search from each node in input order finds
/// going back to a node still on its stack. Reversing them leaves no cycle.
fn back_edges(n: usize, edges: &[(usize, usize, Option<&str>)]) -> Vec<bool> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (e, &(u, _, _)) in edges.iter().enumerate() {
        out[u].push(e);
    }
    // 0: not yet seen, 1: on the stack, 2: done.
    let mut state = vec![0u8; n];
    let mut back = vec![false; edges.len()];
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        let mut stack = vec![(root, 0)];
        while let Some((u, next)) = stack.last_mut() {
            let u = *u;
            let Some(&e) = out[u].get(*next) else {
                state[u] = 2;
                stack.pop();
                continue;
            };
            *next += 1;
            let v = edges[e].1;
            match state[v] {
                0 => {
                    state[v] = 1;
                    stack.push((v, 0));
                }
                1 => back[e] = true,
                _ => {}
            }
        }
    }
    back
}

/// Longest-path layers, sources pulled down next to their first successor
/// so their edges stay short.
fn layers(n: usize, routes: &[Route]) -> Vec<usize> {
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for r in routes {
        preds[r.bottom].push(r.top);
        succs[r.top].push(r.bottom);
    }
    let mut layer = vec![0; n];
    let mut waiting: Vec<usize> = preds.iter().map(Vec::len).collect();
    let mut done = vec![false; n];
    // The graph is acyclic once back-edges are reversed, so some node is
    // always ready; take the first, for determinism.
    while let Some(u) = (0..n).find(|&u| !done[u] && waiting[u] == 0) {
        done[u] = true;
        layer[u] = preds[u].iter().map(|&p| layer[p] + 1).max().unwrap_or(0);
        for &v in &succs[u] {
            waiting[v] -= 1;
        }
    }
    for u in 0..n {
        if preds[u].is_empty()
            && let Some(m) = succs[u].iter().map(|&v| layer[v]).min()
        {
            layer[u] = m - 1;
        }
    }
    layer
}

/// Orders each layer by barycenter sweeps, keeping the order with the
/// fewest crossings. Dummies of reversed edges stay at the end of their
/// layer, so those edges run up the side.
fn order(layers: &mut [Vec<usize>], items: &[Item], segments: &[Segment]) {
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for s in segments {
        down[s.upper].push(s.lower);
        up[s.lower].push(s.upper);
    }
    let side = |i: usize| matches!(items[i].kind, Kind::Dummy { back: true, .. });
    for l in layers.iter_mut() {
        l.sort_by_key(|&i| side(i));
    }
    let mut at = vec![0usize; items.len()];
    let index = |layers: &[Vec<usize>], at: &mut [usize]| {
        for l in layers {
            for (p, &i) in l.iter().enumerate() {
                at[i] = p;
            }
        }
    };
    index(layers, &mut at);
    let mut best = crossings(segments, &at);
    let mut best_layers = layers.to_vec();
    for sweep in 0..SWEEPS {
        let downward = sweep % 2 == 0;
        let ls: Vec<usize> = if downward {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        for l in ls {
            let nbrs = if downward { &up } else { &down };
            let mut keyed: Vec<(bool, f64, usize)> = layers[l]
                .iter()
                .map(|&i| {
                    let k = if nbrs[i].is_empty() {
                        at[i] as f64
                    } else {
                        nbrs[i].iter().map(|&j| at[j] as f64).sum::<f64>() / nbrs[i].len() as f64
                    };
                    (side(i), k, i)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
            layers[l] = keyed.into_iter().map(|k| k.2).collect();
            for (p, &i) in layers[l].iter().enumerate() {
                at[i] = p;
            }
        }
        let c = crossings(segments, &at);
        if c < best {
            best = c;
            best_layers = layers.to_vec();
        }
    }
    layers.clone_from_slice(&best_layers);
}

/// Pairs of segments that cross between adjacent layers.
fn crossings(segments: &[Segment], at: &[usize]) -> usize {
    let mut count = 0;
    for (i, s) in segments.iter().enumerate() {
        for t in &segments[i + 1..] {
            if s.upper == t.upper || s.lower == t.lower {
                continue;
            }
            let same = |x: usize, y: usize| at[x] as isize - at[y] as isize;
            if same(s.upper, t.upper).signum() * same(s.lower, t.lower).signum() < 0 {
                count += 1;
            }
        }
    }
    count
}

/// Positions items along the order axis. Each sweep moves every item of a
/// layer to where its edges to the layer before (or after) would run
/// straight on average, as close as the layer's order and spacing allow.
fn place(items: &mut [Item], layers: &[Vec<usize>], segments: &[Segment]) {
    // Each item's segments to the layer before and after, as the other
    // item, its port offset and this item's port offset.
    let mut up: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); items.len()];
    let mut down: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); items.len()];
    for s in segments {
        down[s.upper].push((s.lower, s.b, s.a));
        up[s.lower].push((s.upper, s.a, s.b));
    }
    let mut pos: Vec<f64> = vec![0.0; items.len()];
    for l in layers {
        let mut p = 0.0;
        for &i in l {
            pos[i] = p;
            p += (items[i].size_o + SPACING) as f64;
        }
    }
    for downward in [true, false, true, false, true] {
        let ls: Vec<usize> = if downward {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        let nbrs = if downward { &up } else { &down };
        for l in ls {
            let mut offset = 0.0;
            let mut targets = Vec::with_capacity(layers[l].len());
            let weights: Vec<f64> = layers[l].iter().map(|&i| weight(&items[i])).collect();
            for &i in &layers[l] {
                let want = if nbrs[i].is_empty() {
                    pos[i]
                } else {
                    let (sum, total) =
                        nbrs[i]
                            .iter()
                            .fold((0.0, 0.0), |(s, t), &(j, theirs, mine)| {
                                let w = weight(&items[j]);
                                (s + w * (pos[j] + theirs as f64 - mine as f64), t + w)
                            });
                    sum / total
                };
                targets.push(want - offset);
                offset += (items[i].size_o + SPACING) as f64;
            }
            let fitted = monotone(&targets, &weights);
            let mut offset = 0.0;
            for (k, &i) in layers[l].iter().enumerate() {
                pos[i] = fitted[k].round() + offset;
                offset += (items[i].size_o + SPACING) as f64;
            }
        }
    }
    let min = pos.iter().copied().fold(f64::INFINITY, f64::min);
    for (it, p) in items.iter_mut().zip(pos) {
        it.pos = (p - min) as usize;
    }
}

/// How much an item's own alignment counts when a layer is crowded. The
/// side channel of reversed edges gives way to the main flow.
fn weight(it: &Item) -> f64 {
    match it.kind {
        Kind::Dummy { back: true, .. } => 1.0,
        _ => 16.0,
    }
}

/// The closest non-decreasing sequence to `targets` in weighted least
/// squares, by pooling adjacent violators.
fn monotone(targets: &[f64], weights: &[f64]) -> Vec<f64> {
    // Blocks of (weighted sum, total weight, count).
    let mut blocks: Vec<(f64, f64, usize)> = Vec::new();
    for (&t, &w) in targets.iter().zip(weights) {
        blocks.push((t * w, w, 1));
        while let [.., (s1, w1, c1), (s2, w2, c2)] = blocks[..] {
            if s1 / w1 <= s2 / w2 {
                break;
            }
            blocks.pop();
            blocks.pop();
            blocks.push((s1 + s2, w1 + w2, c1 + c2));
        }
    }
    blocks
        .into_iter()
        .flat_map(|(s, w, c)| std::iter::repeat_n(s / w, c))
        .collect()
}

/// Spreads the edges meeting each side of a node along that side, ordered
/// by where their other ends sit in their layer, so they leave without
/// crossing. Sets each segment's ends as offsets into its items.
fn ports(items: &[Item], layers: &[Vec<usize>], segments: &mut [Segment]) {
    let mut at = vec![0; items.len()];
    for l in layers {
        for (p, &i) in l.iter().enumerate() {
            at[i] = p;
        }
    }
    // Segments by the item and side they meet: false for the side facing
    // the next layer, true for the side facing the one before.
    let mut sides: Vec<((usize, bool), Vec<usize>)> = Vec::new();
    for (k, s) in segments.iter().enumerate() {
        for key in [(s.upper, false), (s.lower, true)] {
            match sides.iter_mut().find(|(side, _)| *side == key) {
                Some((_, on)) => on.push(k),
                None => sides.push((key, vec![k])),
            }
        }
    }
    for ((item, before), mut on) in sides {
        let it = &items[item];
        let other = |j: usize| {
            let s = &segments[j];
            if before { s.upper } else { s.lower }
        };
        on.sort_by_key(|&j| (at[other(j)], j));
        let k = on.len();
        for (i, &j) in on.iter().enumerate() {
            let p = match it.kind {
                Kind::Node => 1 + (2 * i + 1) * (it.box_o - 2) / (2 * k),
                Kind::Dummy { .. } => it.mid,
            };
            if before {
                segments[j].b = p;
            } else {
                segments[j].a = p;
            }
        }
    }
}

/// The rows a gap between two layers needs.
struct Gap {
    rows: usize,
    /// Rows before the first track, which hold arrowheads pointing back up.
    top: usize,
}

/// A horizontal run in a gap: all of a bent segment, or one leg of a
/// segment that doglegs through a free column.
struct Wire {
    segment: usize,
    /// The columns of its vertical above the run and below it.
    up: usize,
    low: usize,
    /// The first leg of a dogleg, which must turn above this one.
    after: Option<usize>,
}

/// Assigns each bent segment a track in its gap. A run whose upper column
/// is another's lower column turns above it, so the two verticals never
/// share a cell, and runs on one track keep a column apart. Where those
/// rules go round in a circle, as when two edges swap columns, one segment
/// doglegs through a free column instead.
fn tracks(
    items: &[Item],
    layers: &[Vec<usize>],
    segments: &mut [Segment],
    routes: &[Route],
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for l in 0..layers.len().saturating_sub(1) {
        let here: Vec<usize> = (0..segments.len())
            .filter(|&k| items[segments[k].upper].layer == l)
            .collect();
        let bent: Vec<usize> = here
            .iter()
            .copied()
            .filter(|&k| segments[k].a != segments[k].b)
            .collect();
        let mut wires: Vec<Wire> = bent
            .iter()
            .map(|&k| Wire {
                segment: k,
                up: segments[k].a,
                low: segments[k].b,
                after: None,
            })
            .collect();
        let mut forced = vec![false; wires.len()];
        let assigned = loop {
            match assign(&wires, &forced) {
                Ok(t) => break t,
                Err(w) => {
                    let taken: Vec<usize> = here
                        .iter()
                        .flat_map(|&k| [segments[k].a, segments[k].b])
                        .collect();
                    match free(&wires[w], &taken) {
                        Some(c) if wires[w].after.is_none() && wires[w].up != c => {
                            let low = wires[w].low;
                            wires[w].low = c;
                            wires.push(Wire {
                                segment: wires[w].segment,
                                up: c,
                                low,
                                after: Some(w),
                            });
                            forced.push(false);
                        }
                        _ => forced[w] = true,
                    }
                }
            }
        };
        let count = assigned.iter().map(|t| t + 1).max().unwrap_or(0);
        for (w, wire) in wires.iter().enumerate() {
            let s = &mut segments[wire.segment];
            match wire.after {
                None => s.track = assigned[w],
                Some(_) => s.dogleg = Some((wire.up, assigned[w])),
            }
        }
        let up_arrow = bent.iter().any(|&k| {
            routes[segments[k].edge].back && matches!(items[segments[k].upper].kind, Kind::Node)
        });
        let down_arrow = bent.iter().any(|&k| {
            !routes[segments[k].edge].back && matches!(items[segments[k].lower].kind, Kind::Node)
        });
        let top = usize::from(up_arrow);
        let rows = if count == 0 {
            1
        } else {
            count + top + usize::from(down_arrow)
        };
        gaps.push(Gap { rows, top });
    }
    gaps
}

/// Tracks for wires by the left-edge algorithm, track by track, taking each
/// wire whose constraints allow it. A wire marked forced ignores them. On a
/// track where nothing fits, returns the first wire left over.
fn assign(wires: &[Wire], forced: &[bool]) -> Result<Vec<usize>, usize> {
    let span = |w: &Wire| (w.up.min(w.low), w.up.max(w.low));
    let mut todo: Vec<usize> = (0..wires.len()).collect();
    todo.sort_by_key(|&w| (span(&wires[w]), w));
    let mut track: Vec<Option<usize>> = vec![None; wires.len()];
    let mut t = 0;
    while todo.iter().any(|&w| track[w].is_none()) {
        let earlier = |v: usize, track: &[Option<usize>]| track[v].is_some_and(|tv| tv < t);
        let mut last: Option<usize> = None;
        for &w in &todo {
            if track[w].is_some() {
                continue;
            }
            let ready = forced[w]
                || (wires[w].after.is_none_or(|p| earlier(p, &track))
                    && (0..wires.len()).all(|v| {
                        v == w
                            || wires[v].segment == wires[w].segment
                            || wires[v].up != wires[w].low
                            || earlier(v, &track)
                    }));
            let (lo, hi) = span(&wires[w]);
            if ready && last.is_none_or(|h| lo > h + 1) {
                track[w] = Some(t);
                last = Some(hi);
            }
        }
        if last.is_none() {
            return Err(*todo.iter().find(|&&w| track[w].is_none()).unwrap_or(&0));
        }
        t += 1;
    }
    Ok(track.into_iter().map(|t| t.unwrap_or(0)).collect())
}

/// A column strictly between a wire's ends that no segment in the gap
/// uses, as near the middle as possible.
fn free(w: &Wire, taken: &[usize]) -> Option<usize> {
    let (lo, hi) = (w.up.min(w.low), w.up.max(w.low));
    let mid = (lo + hi) / 2;
    (1..hi - lo)
        .map(|d| {
            if d % 2 == 1 {
                mid + d / 2 + 1
            } else {
                mid - d / 2
            }
        })
        .chain([mid])
        .filter(|&c| c > lo && c < hi)
        .find(|c| !taken.iter().any(|t| t.abs_diff(*c) <= 1))
}

/// Line directions in a cell.
const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// Cells under construction, addressed along the order and rank axes.
struct Grid {
    flow: Flow,
    width: usize,
    height: usize,
    lines: Vec<u8>,
    text: Vec<Option<char>>,
}

impl Grid {
    fn new(flow: Flow, span_o: usize, span_r: usize) -> Self {
        let (width, height) = match flow {
            Flow::Down => (span_o, span_r),
            Flow::Right => (span_r, span_o),
        };
        Grid {
            flow,
            width,
            height,
            lines: vec![0; width * height],
            text: vec![None; width * height],
        }
    }

    /// The cell at an order and rank position, as x and y.
    fn xy(&self, o: usize, r: usize) -> (usize, usize) {
        match self.flow {
            Flow::Down => (o, r),
            Flow::Right => (r, o),
        }
    }

    fn area(&self, o: usize, r: usize, size_o: usize, size_r: usize) -> Area {
        let (x, y) = self.xy(o, r);
        let (width, height) = match self.flow {
            Flow::Down => (size_o, size_r),
            Flow::Right => (size_r, size_o),
        };
        Area {
            x,
            y,
            width,
            height,
        }
    }

    /// An arrowhead pointing along the rank axis, forwards or back.
    fn arrow(&self, forward: bool) -> char {
        match (self.flow, forward) {
            (Flow::Down, true) => '▼',
            (Flow::Down, false) => '▲',
            (Flow::Right, true) => '▶',
            (Flow::Right, false) => '◀',
        }
    }

    fn mark(&mut self, x: usize, y: usize, bits: u8) {
        if x < self.width && y < self.height {
            self.lines[y * self.width + x] |= bits;
        }
    }

    /// A polyline through order and rank positions, each leg straight.
    fn line(&mut self, points: &[(usize, usize)]) {
        for leg in points.windows(2) {
            let (p, q) = (self.xy(leg[0].0, leg[0].1), self.xy(leg[1].0, leg[1].1));
            self.straight(p, q);
        }
    }

    fn straight(&mut self, (x0, y0): (usize, usize), (x1, y1): (usize, usize)) {
        if x0 == x1 {
            let (a, b) = (y0.min(y1), y0.max(y1));
            for y in a..=b {
                let mut bits = 0;
                if y > a {
                    bits |= UP;
                }
                if y < b {
                    bits |= DOWN;
                }
                self.mark(x0, y, bits);
            }
        } else {
            let (a, b) = (x0.min(x1), x0.max(x1));
            for x in a..=b {
                let mut bits = 0;
                if x > a {
                    bits |= LEFT;
                }
                if x < b {
                    bits |= RIGHT;
                }
                self.mark(x, y0, bits);
            }
        }
    }

    /// A box's border.
    fn frame(&mut self, a: Area) {
        let (r, b) = (a.x + a.width - 1, a.y + a.height - 1);
        self.straight((a.x, a.y), (r, a.y));
        self.straight((a.x, b), (r, b));
        self.straight((a.x, a.y), (a.x, b));
        self.straight((r, a.y), (r, b));
    }

    fn put(&mut self, o: usize, r: usize, c: char) {
        let (x, y) = self.xy(o, r);
        if x < self.width && y < self.height {
            self.text[y * self.width + x] = Some(c);
        }
    }

    /// Text along a row, at a cell given as x and y.
    fn text(&mut self, x: usize, y: usize, s: &str) {
        for (i, c) in s.chars().enumerate() {
            if x + i < self.width && y < self.height {
                self.text[y * self.width + x + i] = Some(c);
            }
        }
    }

    fn finish(self, boxes: Vec<Area>, notes: Vec<Area>) -> Picture {
        let rows = (0..self.height)
            .map(|y| {
                (0..self.width)
                    .map(|x| {
                        let i = y * self.width + x;
                        self.text[i].unwrap_or_else(|| glyph(self.lines[i]))
                    })
                    .collect()
            })
            .collect();
        Picture {
            width: self.width,
            height: self.height,
            rows,
            boxes,
            notes,
        }
    }
}

/// The box-drawing character for the directions meeting in a cell, with
/// rounded corners.
fn glyph(bits: u8) -> char {
    match bits {
        0 => ' ',
        UP | DOWN | 3 => '│',
        LEFT | RIGHT | 12 => '─',
        6 => '╮',
        10 => '╭',
        5 => '╯',
        9 => '╰',
        7 => '┤',
        11 => '├',
        14 => '┬',
        13 => '┴',
        _ => '┼',
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn draw(spec: Value) -> String {
        layout(&serde_json::from_value(spec).unwrap()).text()
    }

    fn nodes(ids: &[&str]) -> Value {
        ids.iter().map(|id| json!({"id": id})).collect()
    }

    fn edges(pairs: &[(&str, &str)]) -> Value {
        pairs
            .iter()
            .map(|(f, t)| json!({"from": f, "to": t}))
            .collect()
    }

    /// Thirty nodes in six layers of five, each linked to two in the next
    /// layer and a few back to the first.
    fn thirty() -> Value {
        let ids: Vec<String> = (0..30).map(|i| format!("n{i}")).collect();
        let mut pairs = Vec::new();
        for i in 0..25 {
            pairs.push((ids[i].clone(), ids[(i / 5 + 1) * 5 + (i * 2) % 5].clone()));
            pairs.push((
                ids[i].clone(),
                ids[(i / 5 + 1) * 5 + (i * 3 + 1) % 5].clone(),
            ));
        }
        for i in [27, 29] {
            pairs.push((ids[i].clone(), ids[i % 5].clone()));
        }
        let edges: Vec<Value> = pairs
            .iter()
            .map(|(f, t)| json!({"from": f, "to": t}))
            .collect();
        let nodes: Vec<Value> = ids.iter().map(|id| json!({"id": id})).collect();
        json!({"nodes": nodes, "edges": edges})
    }

    fn labelled(direction: &str) -> Value {
        json!({"direction": direction, "nodes": [{"id": "busy"}, {"id": "idle"}], "edges": [
            {"from": "idle", "to": "busy", "label": "start"},
            {"from": "busy", "to": "busy", "label": "tick"},
            {"from": "busy", "to": "idle", "label": "stop"}
        ]})
    }

    #[test]
    fn chain_runs_straight_down() {
        let chain =
            json!({"nodes": nodes(&["a", "b", "c"]), "edges": edges(&[("a", "b"), ("b", "c")])});
        assert_eq!(
            draw(chain),
            "\
╭───╮
│ a │
╰─┬─╯
  ▼
╭───╮
│ b │
╰─┬─╯
  ▼
╭───╮
│ c │
╰───╯"
        );
    }

    #[test]
    fn diamond_fans_out_and_back_in() {
        let diamond = json!({"nodes": nodes(&["top", "left", "right", "bottom"]), "edges": edges(&[
            ("top", "left"), ("top", "right"), ("left", "bottom"), ("right", "bottom")
        ])});
        assert_eq!(
            draw(diamond),
            "      ╭─────╮
      │ top │
      ╰─┬─┬─╯
    ╭───╯ ╰───╮
    ▼         ▼
╭──────╮  ╭───────╮
│ left │  │ right │
╰───┬──╯  ╰───┬───╯
    ╰──╮   ╭──╯
       ▼   ▼
    ╭────────╮
    │ bottom │
    ╰────────╯"
        );
    }

    #[test]
    fn back_edge_runs_up_the_side_pointing_the_right_way() {
        let cycle = json!({"nodes": nodes(&["a", "b", "c"]), "edges": edges(&[("a", "b"), ("b", "c"), ("c", "a")])});
        assert_eq!(
            draw(cycle),
            " ╭───╮
 │ a │
 ╰┬──╯
  │ ▲
  ▼ ╰──╮
╭───╮  │
│ b │  │
╰─┬─╯  │
  ▼ ╭──╯
 ╭──┴╮
 │ c │
 ╰───╯"
        );
    }

    #[test]
    fn self_loops_are_markers_beside_the_box() {
        let loops = json!({"nodes": [{"id": "idle"}, {"id": "busy", "label": "Busy"}], "edges": [
            {"from": "idle", "to": "busy"},
            {"from": "busy", "to": "busy", "label": "tick"},
            {"from": "busy", "to": "busy", "label": "poll"},
            {"from": "idle", "to": "idle"}
        ]});
        let p = layout(&serde_json::from_value(loops).unwrap());
        assert_eq!(
            p.text(),
            "\
╭──────╮
│ idle │ ↺
╰───┬──╯
    ▼
╭──────╮
│ Busy │ ↺ tick, poll
╰──────╯"
        );
        let area = |x, y, width, height| Area {
            x,
            y,
            width,
            height,
        };
        assert_eq!(p.boxes, [area(0, 0, 8, 3), area(0, 4, 8, 3)]);
        assert_eq!(p.notes, [area(9, 1, 1, 1), area(9, 5, 12, 1)]);
    }

    #[test]
    fn labels_sit_beside_their_edges() {
        assert_eq!(
            draw(labelled("down")),
            "\
╭──────╮
│ busy │ ↺ tick
╰─┬────╯
  │  ▲
  │  ╰────╮
  │ stop  │ start
  ▼  ╭────╯
╭────┴─╮
│ idle │
╰──────╯"
        );
    }

    #[test]
    fn right_transposes_the_layout() {
        assert_eq!(
            draw(labelled("right")),
            "\
╭──────╮  stop  ╭──────╮
│      ├───────▶│      │
│ busy │        │ idle │
│      │◀╮     ╭┤      │
╰──────╯ │start│╰──────╯
↺ tick   ╰─────╯"
        );
    }

    #[test]
    fn swapped_columns_dogleg_instead_of_overlapping() {
        let swap = json!({"nodes": nodes(&["a", "b", "c", "d"]), "edges": edges(&[
            ("a", "c"), ("a", "d"), ("b", "c"), ("b", "d")
        ])});
        assert_eq!(
            draw(swap),
            "\
╭───╮  ╭───╮
│ a │  │ b │
╰┬─┬╯  ╰┬─┬╯
 │ ╰──╮ │ │
 │ ╭──┼─╯ │
 │ │  ╰─╮ │
 ▼ ▼    ▼ ▼
╭───╮  ╭───╮
│ c │  │ d │
╰───╯  ╰───╯"
        );
    }

    #[test]
    fn thirty_nodes_lay_out_fast_and_the_same_every_time() {
        let g: GraphNode = serde_json::from_value(thirty()).unwrap();
        let start = std::time::Instant::now();
        let p = layout(&g);
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
        assert_eq!(p.text(), layout(&g).text());
        assert_eq!(p.boxes.len(), 30);
        for (i, a) in p.boxes.iter().enumerate() {
            for b in &p.boxes[i + 1..] {
                let apart = a.x + a.width <= b.x
                    || b.x + b.width <= a.x
                    || a.y + a.height <= b.y
                    || b.y + b.height <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        // Every edge ends in an arrowhead.
        let arrows = p.text().chars().filter(|c| "▼▲".contains(*c)).count();
        assert_eq!(arrows, 52);
    }

    #[test]
    fn empty_graph_is_empty() {
        let p = layout(&serde_json::from_value(json!({"nodes": []})).unwrap());
        assert_eq!((p.width, p.height, p.text()), (0, 0, String::new()));
    }
}
