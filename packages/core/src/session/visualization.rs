//! Portable, declarative visualizations embedded in assistant Markdown.
mod json;

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "centaeris.visual.v1";
pub const MAX_BYTES: usize = 32 * 1024;
pub const MAX_ELEMENTS: usize = 128;
pub const MAX_DEPTH: usize = 8;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
/// Shared host display budget; authoritative persisted answer content is unchanged.
pub const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// The catalog is also exported to the browser; properties are literal strings only.
pub fn catalog() -> Value {
    json!({
        "schema": SCHEMA,
        "limits": {"bytes": MAX_BYTES, "elements": MAX_ELEMENTS, "depth": MAX_DEPTH, "textBytes": MAX_TEXT_BYTES, "answerBytes": MAX_ANSWER_BYTES},
        "components": {
            "Card": {"children": true, "props": {"title": {}, "description": {}}},
            "Group": {"children": true, "props": {"title": {"required": true}, "description": {}}},
            "Sequence": {"children": true, "props": {"title": {}}},
            "Columns": {"children": true, "props": {"title": {}}},
            "Text": {"children": false, "props": {"text": {"required": true}, "role": {"required": true, "enum": ["title", "body", "caption"]}}},
            "Status": {"children": false, "props": {"label": {"required": true}, "tone": {"required": true, "enum": ["neutral", "info", "success", "warning", "error"]}}},
            "Badge": {"children": false, "props": {"text": {"required": true}}},
            "Icon": {"children": false, "props": {"name": {"required": true, "enum": ["checkCircle", "circleAlert", "hardDrive", "gitBranch", "arrowDown"]}}},
            "Divider": {"children": false, "props": {}}
        }
    })
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEnvelope {
    pub schema: String,
    pub spec: VisualSpec,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualSpec {
    pub root: String,
    pub elements: BTreeMap<String, VisualElement>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisualElement {
    #[serde(rename = "type")]
    pub kind: String,
    pub props: BTreeMap<String, String>,
    pub children: Vec<String>,
}

impl VisualEnvelope {
    /// Stable reading order, independent of a host's layout or icon support.
    pub fn text(&self) -> String {
        fn walk(spec: &VisualSpec, id: &str, depth: usize, out: &mut Vec<String>) {
            let element = &spec.elements[id];
            let prefix = "  ".repeat(depth);
            let property = |key: &str| element.props.get(key).map(String::as_str).unwrap_or("");
            for name in ["title", "description"] {
                if let Some(value) = element.props.get(name) {
                    out.push(format!("{prefix}{value}"));
                }
            }
            match element.kind.as_str() {
                "Text" | "Badge" => out.push(format!("{prefix}{}", property("text"))),
                "Status" => out.push(format!(
                    "{prefix}[{}] {}",
                    property("tone"),
                    property("label")
                )),
                "Divider" => out.push(format!("{prefix}---")),
                _ => {}
            }
            for (index, child) in element.children.iter().enumerate() {
                if element.kind == "Sequence" {
                    out.push(format!("{prefix}{}.", index + 1));
                }
                walk(spec, child, depth + 1, out);
            }
        }
        let mut lines = Vec::new();
        walk(&self.spec, &self.spec.root, 0, &mut lines);
        lines.join("\n")
    }
}

pub fn parse_visual(source: &str) -> Result<VisualEnvelope, String> {
    if source.len() > MAX_BYTES {
        return Err("Visualization exceeds the byte limit".into());
    }
    let value = json::parse(source)?;
    let envelope: VisualEnvelope =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if envelope.schema != SCHEMA {
        return Err("Unsupported visualization schema".into());
    }
    let spec = &envelope.spec;
    if spec.elements.is_empty() || spec.elements.len() > MAX_ELEMENTS {
        return Err("Invalid visualization element count".into());
    }
    let catalog = catalog();
    for element in spec.elements.values() {
        let rule = catalog["components"]
            .get(&element.kind)
            .ok_or_else(|| format!("Unknown visualization component: {}", element.kind))?;
        if !rule["children"].as_bool().unwrap_or(false) && !element.children.is_empty() {
            return Err("Leaf components cannot contain children".into());
        }
        let props = rule["props"].as_object().expect("catalog props");
        for (key, value) in &element.props {
            let property = props
                .get(key)
                .ok_or_else(|| format!("Unknown property: {key}"))?;
            if value.len() > MAX_TEXT_BYTES
                || value
                    .chars()
                    .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            {
                return Err("Invalid visualization text".into());
            }
            if property["enum"].as_array().is_some_and(|choices| {
                !choices
                    .iter()
                    .any(|choice| choice.as_str() == Some(value.as_str()))
            }) {
                return Err(format!("Invalid value for {key}"));
            }
        }
        for (key, rule) in props {
            if rule["required"] == true && !element.props.contains_key(key) {
                return Err(format!("Missing property: {key}"));
            }
        }
    }
    fn visit(
        spec: &VisualSpec,
        id: &str,
        depth: usize,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if id.is_empty() {
            return Err("Visualization element IDs must be nonempty".into());
        }
        if depth > MAX_DEPTH {
            return Err("Visualization is too deeply nested".into());
        }
        if !seen.insert(id.to_string()) {
            return Err("Visualization must be a tree with one parent per element".into());
        }
        let element = spec
            .elements
            .get(id)
            .ok_or("Missing visualization element")?;
        for child in &element.children {
            visit(spec, child, depth + 1, seen)?;
        }
        Ok(())
    }
    let mut seen = BTreeSet::new();
    visit(spec, &spec.root, 1, &mut seen)?;
    if seen.len() != spec.elements.len() {
        return Err("Unreachable visualization elements".into());
    }
    Ok(envelope)
}

#[derive(Debug)]
pub struct VisualBlock {
    pub start: usize,
    pub end: usize,
    pub json: String,
    pub complete: bool,
}

/// CommonMark structure prevents quoted, nested and literal examples becoming UI.
pub fn visual_blocks(source: &str) -> Vec<VisualBlock> {
    let mut blocks = Vec::new();
    let mut depth = 0_usize;
    let mut active: Option<VisualBlock> = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0
                    && matches!(&tag, Tag::CodeBlock(CodeBlockKind::Fenced(info)) if info.as_ref() == "centaeris_visual")
                {
                    active = Some(VisualBlock {
                        start: range.start,
                        end: range.end,
                        json: String::new(),
                        complete: false,
                    });
                }
                depth += 1;
            }
            Event::Text(text) if active.is_some() => {
                active.as_mut().expect("active block").json.push_str(&text);
            }
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                if let (TagEnd::CodeBlock, Some(mut block)) = (
                    tag,
                    if tag == TagEnd::CodeBlock {
                        active.take()
                    } else {
                        None
                    },
                ) {
                    block.end = range.end;
                    let slice = &source[block.start..block.end];
                    let mut lines = slice.lines();
                    let opener = lines.next().unwrap_or("").trim_start();
                    let marker = opener.chars().next().unwrap_or(' ');
                    let count = opener.chars().take_while(|ch| *ch == marker).count();
                    block.complete = lines.last().is_some_and(|line| {
                        let trimmed = line.trim_start_matches(' ');
                        line.len() - trimmed.len() <= 3
                            && trimmed.chars().take_while(|ch| *ch == marker).count() >= count
                            && trimmed.trim_matches(marker).trim().is_empty()
                    });
                    blocks.push(block);
                }
            }
            _ => {}
        }
    }
    blocks
}

pub fn project_markdown(source: &str, finalized: bool) -> String {
    project_markdown_range(source, 0..source.len(), finalized).expect("complete source range")
}

/// Project a source-aligned fragment without losing the surrounding Markdown context.
/// A host must retain unfinished visual blocks rather than split them between fragments.
pub fn project_markdown_range(
    source: &str,
    range: std::ops::Range<usize>,
    finalized: bool,
) -> Result<String, String> {
    source
        .get(range.clone())
        .ok_or("Invalid assistant source range")?;
    let mut output = String::new();
    let mut cursor = range.start;
    for block in visual_blocks(source) {
        if block.end <= range.start || block.start >= range.end {
            continue;
        }
        if block.start < range.start || block.end > range.end {
            return Err("Assistant source range splits a visualization".into());
        }
        output.push_str(&source[cursor..block.start]);
        if !block.complete && !finalized {
            output.push_str("[Visualization loading…]\n");
        } else if !block.complete {
            output.push_str("[Visualization interrupted; original source follows]\n");
            output.push_str(&source[block.start..block.end]);
        } else {
            match parse_visual(&block.json) {
                Ok(visual) => {
                    output.push_str(&visual.text());
                    output.push('\n');
                }
                Err(error) => {
                    output.push_str(&format!(
                        "[Invalid visualization: {error}; original source follows]\n"
                    ));
                    output.push_str(&source[block.start..block.end]);
                }
            }
        }
        cursor = block.end;
    }
    output.push_str(&source[cursor..range.end]);
    Ok(output)
}

/// One portable output vocabulary, kept compact for small model context budgets.
pub fn model_instructions() -> String {
    let definition = catalog();
    let vocabulary = definition["components"]
        .as_object()
        .expect("component catalog")
        .iter()
        .map(|(name, rule)| {
            let props = rule["props"]
                .as_object()
                .expect("property catalog")
                .iter()
                .map(|(key, property)| {
                    let optional = if property["required"] == true {
                        ""
                    } else {
                        "?"
                    };
                    let choices = property["enum"]
                        .as_array()
                        .map(|choices| {
                            format!(
                                ":{}",
                                choices
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("|")
                            )
                        })
                        .unwrap_or_default();
                    format!("{key}{optional}{choices}")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{name}{}({props})",
                if rule["children"] == true { "*" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    let example = json!({"schema": SCHEMA, "spec": {"root":"t", "elements": {
        "t":{"type":"Badge","props":{"text":"Example"},"children":[]}
    }}});
    let fence = "\u{60}".repeat(3);
    format!("## Visuals\n\nFor diagrams: ? optional,* container;Columns=layout,Status=text,Icon=decoration.\n{vocabulary}\n{fence}centaeris_visual\n{example}\n{fence}")
}
