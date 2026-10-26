//! Excalidraw drawings (Nextcloud Whiteboard's .whiteboard, and .excalidraw): the scene's JSON
//! turned into SVG, which the image loaders then draw. Shapes, text, arrows, lines, freehand and
//! embedded images; Excalidraw's hand-drawn wobble is drawn straight.

use serde_json::Value;
use std::fmt::Write;

pub fn is_whiteboard(name: &str, ct: &str) -> bool {
    let n = name.to_lowercase();
    ct == "application/vnd.excalidraw+json" || n.ends_with(".whiteboard") || n.ends_with(".excalidraw")
}

fn num(e: &Value, k: &str) -> f64 {
    e.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn s<'a>(e: &'a Value, k: &str) -> &'a str {
    e.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Colours go into attributes: keep only what a colour can contain.
fn color(c: &str) -> String {
    let c: String = c.chars().filter(|ch| ch.is_ascii_alphanumeric() || "#(),. %".contains(*ch)).collect();
    if c.is_empty() || c == "transparent" { "none".into() } else { c }
}

fn esc(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The scene as an SVG document, or None if it isn't an Excalidraw scene.
pub fn to_svg(json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    if s(&v, "type") != "excalidraw" {
        return None;
    }
    let elements: Vec<&Value> = v
        .get("elements")?
        .as_array()?
        .iter()
        .filter(|e| !e.get("isDeleted").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    let files = v.get("files");
    let bg = color(v.pointer("/appState/viewBackgroundColor").and_then(Value::as_str).unwrap_or("#ffffff"));

    // bounds of everything drawn
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for e in &elements {
        let (x, y) = (num(e, "x"), num(e, "y"));
        let mut grow = |px: f64, py: f64| {
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        };
        match e.get("points").and_then(Value::as_array) {
            Some(pts) => pts
                .iter()
                .filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?)))
                .for_each(|(px, py)| grow(x + px, y + py)),
            None => {
                grow(x, y);
                grow(x + num(e, "width"), y + num(e, "height"));
            }
        }
    }
    if x0 > x1 {
        (x0, y0, x1, y1) = (0.0, 0.0, 400.0, 300.0); // an empty board
    }
    let pad = 20.0;
    let (w, h) = (x1 - x0 + 2.0 * pad, y1 - y0 + 2.0 * pad);
    let mut out = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="{} {} {w} {h}" width="{w}" height="{h}"><rect x="{}" y="{}" width="{w}" height="{h}" fill="{bg}"/>"#,
        x0 - pad,
        y0 - pad,
        x0 - pad,
        y0 - pad
    );
    for e in &elements {
        let (x, y, ew, eh) = (num(e, "x"), num(e, "y"), num(e, "width"), num(e, "height"));
        let stroke = color(s(e, "strokeColor"));
        let fill = color(s(e, "backgroundColor"));
        let sw = num(e, "strokeWidth").max(0.5);
        let opacity = e.get("opacity").and_then(Value::as_f64).unwrap_or(100.0) / 100.0;
        // ponytail: hachure / cross-hatch fills drawn as a light solid fill
        let fill_op = if s(e, "fillStyle") == "solid" { 1.0 } else { 0.35 };
        let dash = match s(e, "strokeStyle") {
            "dashed" => format!(r#" stroke-dasharray="{} {}""#, sw * 4.0, sw * 3.0),
            "dotted" => format!(r#" stroke-dasharray="{sw} {}""#, sw * 2.5),
            _ => String::new(),
        };
        let paint =
            format!(r#"stroke="{stroke}" stroke-width="{sw}" fill="{fill}" fill-opacity="{fill_op}"{dash} stroke-linecap="round" stroke-linejoin="round""#);
        let angle = num(e, "angle").to_degrees();
        let _ = write!(out, r#"<g opacity="{opacity}" transform="rotate({angle} {} {})">"#, x + ew / 2.0, y + eh / 2.0);
        let rounded = e.get("roundness").is_some_and(|r| !r.is_null());
        match s(e, "type") {
            "rectangle" | "frame" | "magicframe" => {
                let r = if rounded { (ew.min(eh) / 4.0).min(32.0) } else { 0.0 };
                let _ = write!(out, r#"<rect x="{x}" y="{y}" width="{ew}" height="{eh}" rx="{r}" {paint}/>"#);
            }
            "ellipse" => {
                let _ = write!(
                    out,
                    r#"<ellipse cx="{}" cy="{}" rx="{}" ry="{}" {paint}/>"#,
                    x + ew / 2.0,
                    y + eh / 2.0,
                    ew / 2.0,
                    eh / 2.0
                );
            }
            "diamond" => {
                let (cx, cy) = (x + ew / 2.0, y + eh / 2.0);
                let _ = write!(out, r#"<polygon points="{cx},{y} {},{cy} {cx},{} {x},{cy}" {paint}/>"#, x + ew, y + eh);
            }
            "line" | "arrow" | "freedraw" => {
                let pts: Vec<(f64, f64)> = e
                    .get("points")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|p| Some((x + p.get(0)?.as_f64()?, y + p.get(1)?.as_f64()?))).collect())
                    .unwrap_or_default();
                if pts.len() >= 2 {
                    let list: Vec<String> = pts.iter().map(|(a, b)| format!("{a},{b}")).collect();
                    let closed = s(e, "type") == "line" && pts.first() == pts.last() && fill != "none";
                    let shape = if closed { "polygon" } else { "polyline" };
                    let f = if closed { fill.as_str() } else { "none" };
                    let _ = write!(
                        out,
                        r#"<{shape} points="{}" stroke="{stroke}" stroke-width="{sw}" fill="{f}" fill-opacity="{fill_op}"{dash} stroke-linecap="round" stroke-linejoin="round"/>"#,
                        list.join(" ")
                    );
                    for (head, tip, from) in [("endArrowhead", pts[pts.len() - 1], pts[pts.len() - 2]), ("startArrowhead", pts[0], pts[1])] {
                        if let Some(kind) = e.get(head).and_then(Value::as_str) {
                            out.push_str(&arrowhead(kind, tip, from, sw, &stroke));
                        }
                    }
                }
            }
            "text" => {
                let size = num(e, "fontSize").max(1.0);
                let lh = e.get("lineHeight").and_then(Value::as_f64).unwrap_or(1.25) * size;
                let family = match e.get("fontFamily").and_then(Value::as_i64) {
                    Some(3) => "monospace",
                    Some(2) | Some(6) => "sans-serif",
                    _ => "Comic Neue, Comic Sans MS, sans-serif", // the hand-drawn ones
                };
                let (anchor, tx) = match s(e, "textAlign") {
                    "center" => ("middle", x + ew / 2.0),
                    "right" => ("end", x + ew),
                    _ => ("start", x),
                };
                let _ = write!(
                    out,
                    r#"<text font-size="{size}" font-family="{family}" fill="{stroke}" text-anchor="{anchor}">"#
                );
                for (i, line) in s(e, "text").lines().enumerate() {
                    let _ = write!(out, r#"<tspan x="{tx}" y="{}">{}</tspan>"#, y + lh * i as f64 + size * 0.85, esc(line));
                }
                out.push_str("</text>");
            }
            "image" => {
                let id = s(e, "fileId");
                if let Some(url) = files
                    .and_then(|f| f.pointer(&format!("/{id}/dataURL")))
                    .and_then(Value::as_str)
                    .filter(|u| u.starts_with("data:image/"))
                {
                    let _ = write!(
                        out,
                        r#"<image x="{x}" y="{y}" width="{ew}" height="{eh}" preserveAspectRatio="none" href="{}"/>"#,
                        esc(url)
                    );
                }
            }
            _ => {}
        }
        out.push_str("</g>");
    }
    out.push_str("</svg>");
    Some(out)
}

/// An arrowhead at `tip`, pointing away from `from`.
fn arrowhead(kind: &str, tip: (f64, f64), from: (f64, f64), sw: f64, stroke: &str) -> String {
    let a = (tip.1 - from.1).atan2(tip.0 - from.0);
    let len = 10.0 + sw * 3.0;
    let wing = |d: f64| (tip.0 - len * (a + d).cos(), tip.1 - len * (a + d).sin());
    let (l, r) = (wing(0.45), wing(-0.45));
    match kind {
        "triangle" | "triangle_outline" => {
            let f = if kind == "triangle" { stroke } else { "none" };
            format!(
                r#"<polygon points="{},{} {},{} {},{}" fill="{f}" stroke="{stroke}" stroke-width="{sw}"/>"#,
                tip.0, tip.1, l.0, l.1, r.0, r.1
            )
        }
        "dot" | "circle" | "circle_outline" => {
            let f = if kind == "circle_outline" { "none" } else { stroke };
            format!(
                r#"<circle cx="{}" cy="{}" r="{}" fill="{f}" stroke="{stroke}" stroke-width="{sw}"/>"#,
                tip.0,
                tip.1,
                3.0 + sw
            )
        }
        "bar" => {
            let (b1, b2) = (wing(std::f64::consts::FRAC_PI_2), wing(-std::f64::consts::FRAC_PI_2));
            format!(
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{sw}"/>"#,
                b1.0, b1.1, b2.0, b2.1
            )
        }
        _ => format!(
            r#"<polyline points="{},{} {},{} {},{}" fill="none" stroke="{stroke}" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round"/>"#,
            l.0, l.1, tip.0, tip.1, r.0, r.1
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_to_svg() {
        let scene = r##"{"type":"excalidraw","elements":[
            {"type":"rectangle","x":0,"y":0,"width":100,"height":50,"strokeColor":"#1e1e1e","backgroundColor":"#ffc9c9","fillStyle":"solid","strokeWidth":2,"roundness":{"type":3}},
            {"type":"text","x":10,"y":10,"width":80,"height":25,"text":"Hello <world>\nline two","fontSize":20,"fontFamily":1,"textAlign":"center","strokeColor":"#1e1e1e"},
            {"type":"arrow","x":100,"y":25,"width":50,"height":0,"points":[[0,0],[50,0]],"endArrowhead":"arrow","strokeColor":"#1e1e1e"},
            {"type":"ellipse","x":0,"y":0,"width":10,"height":10,"isDeleted":true},
            {"type":"diamond","x":0,"y":60,"width":40,"height":40,"strokeColor":"red\" onload=\"x","backgroundColor":"transparent"}
        ],"appState":{"viewBackgroundColor":"#ffffff"}}"##;
        let svg = to_svg(scene).unwrap();
        assert!(svg.contains("<rect x=\"0\" y=\"0\" width=\"100\"") && svg.contains("rx=\"12.5\""), "{svg}");
        assert!(svg.contains("Hello &lt;world&gt;") && svg.contains("line two"));
        assert!(svg.contains("<polygon points=\"20,60")); // the diamond
        assert!(!svg.contains("ellipse")); // deleted
        assert!(!svg.contains("\" onload")); // colours can't break out of their attribute
        assert!(svg.contains("viewBox=\"-20 -20 190 140\""), "{svg}");
        // GTK draws it
        let t = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(svg.into_bytes()));
        assert!(t.is_ok(), "{t:?}");
        assert!(to_svg(r#"{"type":"something-else"}"#).is_none());
        assert!(is_whiteboard("Venn diagram.whiteboard", "text/plain") && is_whiteboard("x.excalidraw", ""));
    }
}
