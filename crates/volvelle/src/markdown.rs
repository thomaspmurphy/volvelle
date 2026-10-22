//! A deliberately small Markdown subset: headings, bullets, numbered items,
//! quotes, fenced code, and inline **bold**, *italic* and `code`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::draw::palette;

pub fn lines(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(Line::from(Span::styled(
                format!("  {raw}"),
                palette::muted(),
            )));
            continue;
        }
        let trimmed = raw.trim_start();
        let indent = &raw[..raw.len() - trimmed.len()];
        let line = if let Some(rest) = heading(trimmed) {
            let (level, title) = rest;
            let style = palette::accent().add_modifier(Modifier::BOLD);
            let style = if level == 1 {
                style.add_modifier(Modifier::UNDERLINED)
            } else {
                style
            };
            Line::from(Span::styled(title.to_string(), style))
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = vec![Span::raw(format!("{indent}• "))];
            spans.extend(inline(rest, Style::new()));
            Line::from(spans)
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", palette::muted())];
            spans.extend(inline(rest, palette::muted().italic()));
            Line::from(spans)
        } else {
            let mut spans = vec![Span::raw(indent.to_string())];
            spans.extend(inline(trimmed, Style::new()));
            Line::from(spans)
        };
        out.push(line);
    }
    out
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&level) {
        line[level..].strip_prefix(' ').map(|t| (level, t))
    } else {
        None
    }
}

fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let marker = ["**", "`", "*"].into_iter().find(|m| rest.starts_with(m));
        if let Some(m) = marker
            && let Some(end) = rest[m.len()..].find(m)
            && end > 0
        {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), base));
            }
            let inner = rest[m.len()..m.len() + end].to_string();
            let style = match m {
                "**" => base.add_modifier(Modifier::BOLD),
                "`" => palette::code(),
                _ => base.add_modifier(Modifier::ITALIC),
            };
            spans.push(Span::styled(inner, style));
            rest = &rest[2 * m.len() + end..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        buf.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, base));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn renders_subset() {
        let md = "# Title\n- **bold** and `code`\n```\nlet x = 1;\n```\nplain *it*";
        let out = lines(md);
        assert_eq!(plain(&out[0]), "Title");
        assert_eq!(plain(&out[1]), "• bold and code");
        assert_eq!(plain(&out[2]), "  let x = 1;");
        assert_eq!(plain(&out[3]), "plain it");
    }
}
