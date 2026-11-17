//! SVG drawing. Text stays in `<text>` elements.

use crate::ontology::graph::{EdgeSource, Graph, NodeKind};
use crate::ontology::layout::Layout;
use crate::ontology::schema::RelationStatus;

pub fn to_svg(graph: &Graph, layout: &Layout, title: &str) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">\n",
        num(layout.width),
        num(layout.height),
        num(layout.width),
        num(layout.height)
    ));
    line(
        &mut out,
        1,
        &format!(
            "<text x=\"16.0\" y=\"24.0\" font-family=\"{FONT}\" font-size=\"13\" font-weight=\"700\" fill=\"#111827\">{}</text>",
            escape(title)
        ),
    );
    let mut nodes: Vec<_> = layout.nodes.iter().collect();
    nodes.sort_by(|left, right| left.key.cmp(&right.key));
    for placed in nodes {
        let node = &graph.nodes[&placed.key];
        line(
            &mut out,
            1,
            &format!("<g id=\"node-{}\">", escape(&node.title)),
        );
        let missing = matches!(node.kind, NodeKind::Missing);
        let dash = if missing {
            " stroke=\"#9ca3af\" stroke-dasharray=\"4 3\""
        } else {
            " stroke=\"#111827\""
        };
        line(
            &mut out,
            2,
            &format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"6\" fill=\"#ffffff\" stroke-width=\"1.5\"{dash}/>",
                num(placed.rect.x),
                num(placed.rect.y),
                num(placed.rect.w),
                num(placed.rect.h)
            ),
        );
        write_text(
            &mut out,
            2,
            placed.rect.x + 12.0,
            placed.rect.y + 18.0,
            &node_text(node),
            true,
        );
        line(&mut out, 1, "</g>");
    }
    for placed in &layout.edges {
        let edge = &graph.edges[placed.index];
        let (status, kind) = edge_attrs(edge);
        line(
            &mut out,
            1,
            &format!(
                "<g id=\"edge-{}\" data-status=\"{status}\" data-kind=\"{kind}\">",
                placed.index
            ),
        );
        let mut path = String::new();
        for (index, point) in placed.points.iter().enumerate() {
            if index == 0 {
                path.push_str(&format!("M {} {}", num(point.0), num(point.1)));
            } else {
                path.push_str(&format!(" L {} {}", num(point.0), num(point.1)));
            }
        }
        let dash = if status == "draft" {
            " stroke-dasharray=\"6 4\""
        } else {
            ""
        };
        let (color, width) = if kind == "rollup" {
            ("#b45309", "2")
        } else {
            ("#374151", "1.5")
        };
        line(
            &mut out,
            2,
            &format!(
                "<path d=\"{path}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"{width}\"{dash}/>"
            ),
        );
        if let Some(polygon) = arrow(&placed.points) {
            line(
                &mut out,
                2,
                &format!("<polygon points=\"{polygon}\" fill=\"{color}\" stroke=\"{color}\"/>"),
            );
        }
        line(
            &mut out,
            2,
            &format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#ffffff\" stroke=\"none\"/>",
                num(placed.label_rect.x),
                num(placed.label_rect.y),
                num(placed.label_rect.w),
                num(placed.label_rect.h)
            ),
        );
        write_text(
            &mut out,
            2,
            placed.label_rect.x + 4.0,
            placed.label_rect.y + 12.0,
            &placed.label_lines,
            false,
        );
        line(&mut out, 1, "</g>");
    }
    write_legend(&mut out, layout.height);
    out.push_str("</svg>\n");
    out
}

const FONT: &str = "Malgun Gothic, Apple SD Gothic Neo, Noto Sans KR, sans-serif";

fn node_text(node: &crate::ontology::graph::Node) -> Vec<String> {
    let mut lines = vec![node.title.clone()];
    match &node.kind {
        NodeKind::Missing => lines.push("(not pulled)".to_string()),
        NodeKind::Group { members } => lines.push(format!("{} tables", members.len())),
        NodeKind::Pulled => {
            if let Some(comment) = &node.comment {
                lines.push(clip(comment, 40));
            }
            if !node.primary_key.is_empty() {
                lines.push(clip(&format!("PK: {}", node.primary_key.join(", ")), 40));
            }
        }
    }
    lines
}

fn edge_attrs(edge: &crate::ontology::graph::Edge) -> (&'static str, &'static str) {
    let status = match &edge.source {
        EdgeSource::ForeignKey { .. } => "foreign_key",
        EdgeSource::Relation {
            status: RelationStatus::Draft,
        } => "draft",
        EdgeSource::Relation {
            status: RelationStatus::Confirmed,
        } => "confirmed",
    };
    let kind = match edge.kind {
        crate::ontology::schema::RelationKind::Rollup => "rollup",
        crate::ontology::schema::RelationKind::Reference => "reference",
    };
    (status, kind)
}

fn write_text(out: &mut String, depth: usize, x: f32, y: f32, lines: &[String], bold_first: bool) {
    if lines.is_empty() {
        return;
    }
    let weight = if bold_first { "700" } else { "400" };
    let mut body = format!(
        "<text x=\"{}\" y=\"{}\" font-family=\"{FONT}\" font-size=\"12\" font-weight=\"{weight}\" fill=\"#111827\">{}",
        num(x),
        num(y),
        escape(&lines[0])
    );
    for (index, line_text) in lines.iter().skip(1).enumerate() {
        body.push_str(&format!(
            "<tspan x=\"{}\" dy=\"18\">{}</tspan>",
            num(x),
            escape(line_text)
        ));
        let _ = index;
    }
    body.push_str("</text>");
    line(out, depth, &body);
}

fn write_legend(out: &mut String, height: f32) {
    let y = snap_local(height - 28.0);
    let y_text = num(y);
    let text_y = num(y + 4.0);
    line(out, 1, "<g id=\"legend\">");
    line(
        out,
        2,
        &format!(
            "<path d=\"M 16.0 {y_text} L 40.0 {y_text}\" fill=\"none\" stroke=\"#374151\" stroke-width=\"1.5\"/>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<text x=\"44.0\" y=\"{text_y}\" font-family=\"{FONT}\" font-size=\"12\" fill=\"#111827\">confirmed</text>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<path d=\"M 140.0 {y_text} L 164.0 {y_text}\" fill=\"none\" stroke=\"#374151\" stroke-width=\"1.5\" stroke-dasharray=\"6 4\"/>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<text x=\"168.0\" y=\"{text_y}\" font-family=\"{FONT}\" font-size=\"12\" fill=\"#111827\">draft</text>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<path d=\"M 220.0 {y_text} L 244.0 {y_text}\" fill=\"none\" stroke=\"#374151\" stroke-width=\"1.5\"/>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<text x=\"248.0\" y=\"{text_y}\" font-family=\"{FONT}\" font-size=\"12\" fill=\"#111827\">FK</text>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<path d=\"M 280.0 {y_text} L 304.0 {y_text}\" fill=\"none\" stroke=\"#b45309\" stroke-width=\"2\"/>"
        ),
    );
    line(
        out,
        2,
        &format!(
            "<text x=\"308.0\" y=\"{text_y}\" font-family=\"{FONT}\" font-size=\"12\" fill=\"#111827\">rollup</text>"
        ),
    );
    line(out, 1, "</g>");
}

fn arrow(points: &[(f32, f32)]) -> Option<String> {
    let (from, tip) = points.windows(2).last().map(|pair| (pair[0], pair[1]))?;
    let dx = tip.0 - from.0;
    let dy = tip.1 - from.1;
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;
    let base = (tip.0 - ux * 8.0, tip.1 - uy * 8.0);
    let points = [
        tip,
        (base.0 + px * 3.5, base.1 + py * 3.5),
        (base.0 - px * 3.5, base.1 - py * 3.5),
    ];
    Some(
        points
            .iter()
            .map(|point| format!("{},{}", num(point.0), num(point.1)))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn clip(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        text.to_string()
    } else {
        let mut shortened: String = chars.into_iter().take(max - 1).collect();
        shortened.push('…');
        shortened
    }
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn line(out: &mut String, depth: usize, text: &str) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(text);
    out.push('\n');
}

fn num(value: f32) -> String {
    format!("{:.1}", snap_local(value))
}

fn snap_local(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}
