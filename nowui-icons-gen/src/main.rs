//! Dev tool (not shipped in any NowUI app binary — `publish = false`):
//! regenerates `nowui-icons/assets/icons.nowdat` from an **extracted**
//! `react-icons` npm package tarball (`npm pack react-icons` /
//! `curl .../react-icons-<ver>.tgz | tar xz`, or a `node_modules/react-icons`
//! checkout — anything with `<set>/index.js` files under it).
//!
//! react-icons doesn't ship raw `.svg` files; each set (`fa`, `md`, `bs`, ...)
//! is one generated JS module where every icon is a
//! `module.exports.IconName = function IconName (props) { return
//! GenIcon({"tag":"svg","attr":{...},"child":[...]})(props); };` call — the
//! `GenIcon(...)` argument is itself a small, valid JSON document: a
//! `{tag, attr, child}` tree that's structurally identical to a parsed SVG
//! DOM (see `packages/react-icons/lib/iconBase.js` in the upstream repo).
//! This tool line-scans each set's `index.js`, brace-matches out that JSON
//! blob per icon (`extract_json_arg`), parses it, and re-serializes the tree
//! as real SVG XML (`render_svg`) — no JS engine, no npm/node dependency,
//! just `serde_json` over text `react-icons` already ships as valid JSON
//! embedded in JS.
//!
//! react-icons' own `IconBase` component (see the module doc comment above)
//! applies `stroke="currentColor" fill="currentColor" strokeWidth="0"` as
//! defaults on the root `<svg>` at render time — not present in the JSON
//! itself — so `render_svg` injects the same three defaults (recoloring at
//! runtime, in `nowui-icons`, replaces `currentColor` textually).
//!
//! Usage: `nowui-icons-gen <extracted-react-icons-dir> <output.nowdat> [set...]`
//! (sets default to `fa fa6 md bs io5` if none are given).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

const DEFAULT_SETS: &[&str] = &["fa", "fa6", "md", "bs", "io5"];

/// camelCase JSON attribute names (as react-icons' generator wrote them)
/// that are genuine kebab-case SVG presentation attributes. Every other key
/// is assumed to already be a valid SVG/XML attribute name as-is (`d`, `cx`,
/// `viewBox`, `transform`, `width`, `height`, `x`, `y`, `fill`, `opacity`,
/// `baseProfile`, ...) — real SVG mixes camelCase and hyphenated names, so
/// this is a small, explicit allowlist rather than a blanket conversion.
fn map_attr_name(name: &str) -> String {
    match name {
        "clipRule" => "clip-rule".to_string(),
        "fillRule" => "fill-rule".to_string(),
        "fillOpacity" => "fill-opacity".to_string(),
        "strokeWidth" => "stroke-width".to_string(),
        "strokeLinecap" => "stroke-linecap".to_string(),
        "strokeLinejoin" => "stroke-linejoin".to_string(),
        "strokeMiterlimit" => "stroke-miterlimit".to_string(),
        "strokeDasharray" => "stroke-dasharray".to_string(),
        "strokeOpacity" => "stroke-opacity".to_string(),
        "stopColor" => "stop-color".to_string(),
        "stopOpacity" => "stop-opacity".to_string(),
        other => other.to_string(),
    }
}

fn escape_xml_attr(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Renders a `{tag, attr, child}` JSON tree (react-icons' own shape — see
/// this file's module doc comment) into real SVG/XML text.
fn render_node(node: &serde_json::Value, out: &mut String) {
    let Some(tag) = node.get("tag").and_then(|t| t.as_str()) else { return };
    out.push('<');
    out.push_str(tag);

    let attrs = node.get("attr").and_then(|a| a.as_object());

    if tag == "svg" {
        out.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
        // react-icons' `IconBase` applies these three as defaults at render
        // time (see module doc comment), with the icon's own `data.attr`
        // spread in *after* — so an icon that explicitly sets one of these
        // itself (e.g. a stroke-only glyph with `fill="none"`) overrides
        // the default rather than colliding with it. `usvg` is a strict
        // XML parser and rejects a genuinely duplicate attribute outright,
        // so each default is only emitted when the icon's own `attr` map
        // doesn't already define it.
        let has = |key: &str| attrs.is_some_and(|a| a.contains_key(key));
        if !has("fill") {
            out.push_str(" fill=\"currentColor\"");
        }
        if !has("stroke") {
            out.push_str(" stroke=\"currentColor\"");
        }
        if !has("strokeWidth") {
            out.push_str(" stroke-width=\"0\"");
        }
    }

    if let Some(attrs) = attrs {
        // Sorted for deterministic output (stable diffs on regeneration).
        let mut keys: Vec<&String> = attrs.keys().collect();
        keys.sort();
        for key in keys {
            let Some(value) = attrs[key].as_str() else { continue };
            out.push(' ');
            out.push_str(&map_attr_name(key));
            out.push_str("=\"");
            out.push_str(&escape_xml_attr(value));
            out.push('"');
        }
    }

    let children = node.get("child").and_then(|c| c.as_array()).filter(|c| !c.is_empty());
    match children {
        Some(kids) => {
            out.push('>');
            for kid in kids {
                render_node(kid, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        None => out.push_str("/>"),
    }
}

fn render_svg(root: &serde_json::Value) -> String {
    let mut out = String::new();
    render_node(root, &mut out);
    out
}

/// Given `src[start..]` where `src[start] == '{'`, returns the byte range of
/// the balanced `{...}` object (brace-matched, respecting JSON string
/// quoting/escapes so a literal `{`/`}` inside an SVG path's `"d"` string —
/// which never happens for path data, but could for other icon sets' string
/// attributes in principle — can't desync the match).
fn extract_json_arg(src: &str, start: usize) -> Option<&str> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&src[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Parses one set's `index.js`, returning `(IconName, svg_text)` pairs.
fn parse_set(js: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = js.lines().enumerate().peekable();
    while let Some((_, line)) = lines.next() {
        let Some(rest) = line.strip_prefix("module.exports.") else { continue };
        let Some(name_end) = rest.find(' ') else { continue };
        let name = &rest[..name_end];

        // The `GenIcon({...})` call is on the next line in every observed
        // react-icons module; if that ever changes upstream, this icon is
        // just skipped (reported at the end) rather than panicking.
        let Some((_, next_line)) = lines.peek().copied() else { continue };
        let Some(call_start) = next_line.find("GenIcon(") else { continue };
        let json_start = call_start + "GenIcon(".len();
        let Some(json_text) = extract_json_arg(next_line, json_start) else { continue };

        match serde_json::from_str::<serde_json::Value>(json_text) {
            Ok(tree) => out.push((name.to_string(), render_svg(&tree))),
            Err(_) => continue,
        }
    }
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: nowui-icons-gen <extracted-react-icons-dir> <output.nowdat> [set...]");
        return ExitCode::FAILURE;
    }
    let package_dir = Path::new(&args[1]);
    let output = &args[2];
    let sets: Vec<&str> = if args.len() > 3 { args[3..].iter().map(String::as_str).collect() } else { DEFAULT_SETS.to_vec() };

    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for set in &sets {
        let path = package_dir.join(set).join("index.js");
        let js = match std::fs::read_to_string(&path) {
            Ok(js) => js,
            Err(e) => {
                eprintln!("error: reading {} ({e})", path.display());
                return ExitCode::FAILURE;
            }
        };
        let icons = parse_set(&js);
        if icons.is_empty() {
            eprintln!("warning: found zero icons in {} — is this really a react-icons set?", path.display());
        }
        for (name, svg) in icons {
            if let Some(existing) = entries.insert(name.clone(), svg.into_bytes()) {
                eprintln!("warning: `{name}` is defined in more than one set — keeping the last one seen ({} bytes overwritten)", existing.len());
            }
        }
        println!("parsed {set}: running total {} icons", entries.len());
    }

    let built: Vec<(String, Vec<u8>)> = entries.into_iter().collect();
    let bytes = nowui_image::nowdat::build(&built);
    let total_bytes = bytes.len();
    if let Some(parent) = Path::new(output).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("error creating {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
    }
    if let Err(e) = std::fs::write(output, bytes) {
        eprintln!("error writing `{output}`: {e}");
        return ExitCode::FAILURE;
    }

    println!("wrote `{output}`: {} icons, {total_bytes} bytes", built.len());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_arg_finds_the_balanced_object() {
        let src = r#"return GenIcon({"tag":"svg","attr":{"viewBox":"0 0 1 1"},"child":[]})(props);"#;
        let start = src.find('{').unwrap();
        let json = extract_json_arg(src, start).unwrap();
        assert_eq!(json, r#"{"tag":"svg","attr":{"viewBox":"0 0 1 1"},"child":[]}"#);
    }

    #[test]
    fn render_svg_reconstructs_a_simple_icon() {
        let tree: serde_json::Value = serde_json::from_str(
            r#"{"tag":"svg","attr":{"viewBox":"0 0 512 512"},"child":[{"tag":"path","attr":{"d":"M1 2"},"child":[]}]}"#,
        )
        .unwrap();
        let svg = render_svg(&tree);
        assert!(svg.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" fill="currentColor" stroke="currentColor" stroke-width="0""#));
        assert!(svg.contains(r#"viewBox="0 0 512 512""#));
        assert!(svg.contains(r#"<path d="M1 2"/>"#));
        assert!(svg.ends_with("</svg>"));
    }

    #[test]
    fn render_svg_lets_the_icons_own_attr_override_the_injected_defaults() {
        let tree: serde_json::Value =
            serde_json::from_str(r#"{"tag":"svg","attr":{"viewBox":"0 0 1 1","fill":"none"},"child":[]}"#).unwrap();
        let svg = render_svg(&tree);
        assert_eq!(svg.matches("fill=").count(), 1, "must not emit a duplicate `fill` attribute: {svg}");
        assert!(svg.contains(r#"fill="none""#));
        assert!(svg.contains(r#"stroke="currentColor""#));
    }

    #[test]
    fn render_svg_converts_camel_case_presentation_attributes() {
        let tree: serde_json::Value =
            serde_json::from_str(r#"{"tag":"path","attr":{"fillRule":"evenodd","clipRule":"evenodd","d":"M0 0"},"child":[]}"#).unwrap();
        let svg = render_svg(&tree);
        assert!(svg.contains(r#"fill-rule="evenodd""#));
        assert!(svg.contains(r#"clip-rule="evenodd""#));
    }

    #[test]
    fn parse_set_extracts_every_icon_in_a_tiny_fixture_module() {
        let js = "// THIS FILE IS AUTO GENERATED\nvar GenIcon = require('../lib').GenIcon\nmodule.exports.FaTest = function FaTest (props) {\n  return GenIcon({\"tag\":\"svg\",\"attr\":{\"viewBox\":\"0 0 1 1\"},\"child\":[{\"tag\":\"path\",\"attr\":{\"d\":\"M0 0\"},\"child\":[]}]})(props);\n};\nmodule.exports.FaTest2 = function FaTest2 (props) {\n  return GenIcon({\"tag\":\"svg\",\"attr\":{\"viewBox\":\"0 0 2 2\"},\"child\":[]})(props);\n};\n";
        let icons = parse_set(js);
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0].0, "FaTest");
        assert!(icons[0].1.contains(r#"viewBox="0 0 1 1""#));
        assert_eq!(icons[1].0, "FaTest2");
    }
}
