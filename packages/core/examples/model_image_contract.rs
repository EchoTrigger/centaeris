//! Emit the Core-owned image prompt corpus consumed by cross-language hosts.
use base64::Engine as _;
use centaeris_core::model::prepared_prompt::{
    inspect_model_input_image, ModelInputImageV1, ModelMessageRoleV1, ModelMessageV1,
    PreparedPromptV1, MODEL_INPUT_IMAGE_MAX_BYTES, MODEL_INPUT_IMAGE_MAX_PIXELS,
};
use centaeris_core::tool::ModelToolChoice;
use serde_json::{json, Value};
use std::io::Cursor;

fn corpus() -> Value {
    let cases: Vec<Value> = [image::ImageFormat::Png, image::ImageFormat::Jpeg, image::ImageFormat::WebP]
        .into_iter()
        .map(|format| {
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, format).expect("fixture encoding");
            let (content_type, width, height) = inspect_model_input_image(bytes.get_ref()).expect("valid image");
            let mut prompt = PreparedPromptV1::new(Some("image contract".into()), vec![ModelMessageV1 {
                message_id: "user-1".into(), role: ModelMessageRoleV1::User,
                content: "before [one] middle [two] after".into(), tool_calls: vec![], tool_call_id: None, reasoning_content: None,
            }], vec![], ModelToolChoice::None, 64).expect("valid prompt");
            prompt.set_input_images(["[two]", "[one]"].into_iter().map(|placeholder| ModelInputImageV1 {
                message_id: "user-1".into(), content_type: content_type.into(), placeholder: placeholder.into(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes.get_ref()),
            }).collect()).expect("valid image bindings");
            json!({"contentType": content_type, "widthPx": width, "heightPx": height, "preparedPrompt": prompt})
        }).collect();
    json!({"schema": "model_image_contract.v1", "maxImageBytes": MODEL_INPUT_IMAGE_MAX_BYTES,
        "maxImagePixels": MODEL_INPUT_IMAGE_MAX_PIXELS, "cases": cases})
}

fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&corpus()).expect("serialize corpus")
    );
}

#[test]
fn checked_in_corpus_matches_core_construction_and_validation() {
    let checked_in: Value =
        serde_json::from_str(include_str!("../tests/fixtures/model_images.json"))
            .expect("image corpus");
    assert_eq!(
        corpus(),
        checked_in,
        "regenerate with cargo run -p centaeris-core --example model_image_contract"
    );
}
