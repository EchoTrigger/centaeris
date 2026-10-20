use centaeris_core::session::visualization::{parse_visual, project_markdown, visual_blocks};
use serde_json::Value;
fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/visualizations.json")).unwrap()
}
#[test]
fn visualizations_preserve_reference_information_and_reject_invalid_contracts() {
    for case in fixtures() {
        let result = parse_visual(case["source"].as_str().unwrap());
        assert_eq!(
            result.is_ok(),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if let Ok(visual) = result {
            let text = visual.text();
            let mut position = 0;
            for value in case["contains"].as_array().unwrap() {
                let word = value.as_str().unwrap();
                position += text[position..].find(word).expect(word) + word.len();
            }
        }
    }
}
#[test]
fn visualizations_only_extract_top_level_fences_and_keep_literal_examples() {
    let source = fixtures()[0]["source"].as_str().unwrap().to_string();
    let fence = "\u{60}".repeat(3);
    let outer = "\u{60}".repeat(4);
    let active = format!("Before\n\n{fence}centaeris_visual\n{source}\n{fence}\n\nAfter");
    let blocks = visual_blocks(&active);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].complete);
    assert_eq!(blocks[0].json.trim(), source);
    for literal in [
        format!("{outer}text\n{active}\n{outer}"),
        format!("> {fence}centaeris_visual\n> {source}\n> {fence}"),
        format!("    {fence}centaeris_visual\n    {source}\n    {fence}"),
        format!("- Example:\n\n  {fence}centaeris_visual\n  {source}\n  {fence}"),
    ] {
        assert!(visual_blocks(&literal).is_empty(), "{literal}");
    }
}
#[test]
fn visualizations_project_complete_and_interrupted_streams_from_original_source() {
    let source = fixtures()[0]["source"].as_str().unwrap().to_string();
    let fence = "\u{60}".repeat(3);
    let pending = format!("Before\n\n{fence}centaeris_visual\n{source}");
    let preview = project_markdown(&pending, false);
    assert!(preview.contains("Before"));
    assert!(!preview.contains("\"elements\""));
    assert!(project_markdown(&pending, true).contains(&source));
    let text = project_markdown(&format!("{pending}\n{fence}\n\nAfter"), true);
    assert!(text.contains("Run A") && text.contains("Run B") && text.contains("After"));
    assert!(!text.contains("\"elements\""));
}

#[test]
fn visualization_prompt_exposes_the_portable_shape_and_generated_vocabulary() {
    let prompt = centaeris_core::model::prompt::system::render_system_prompt().unwrap();
    assert!(prompt
        .content
        .contains(centaeris_core::session::visualization::SCHEMA));
    assert!(prompt.content.contains("centaeris_visual"));
    let blocks = visual_blocks(&prompt.content);
    assert_eq!(blocks.len(), 1);
    parse_visual(&blocks[0].json).unwrap();
    let catalog = centaeris_core::session::visualization::catalog();
    for name in catalog["components"].as_object().unwrap().keys() {
        assert!(prompt.content.contains(name), "Missing component: {name}");
    }
}
