use centaeris_core::session::visualization::{catalog, parse_visual};
use serde_json::{json, Value};
fn main() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../tests/fixtures/visualizations.json")).unwrap();
    let cases: Vec<Value> = fixtures.iter().map(|case| {
        let result = parse_visual(case["source"].as_str().unwrap());
        json!({"name": case["name"], "source": case["source"], "valid": result.is_ok(), "text": result.ok().map(|visual| visual.text())})
    }).collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"catalog": catalog(), "cases": cases})).unwrap()
    );
}
