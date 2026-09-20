use centaeris_core::session::transcript::*;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--samples") {
        let text = TranscriptTextContentV1::inline("sample".into());
        let reference = TranscriptContentRefV1 {
            ref_id: "ref".into(),
            revision: "1".into(),
            byte_length: "6".into(),
        };
        let bodies = vec![
            TranscriptBlockBodyV1::UserText {
                content: text.clone(),
            },
            TranscriptBlockBodyV1::AssistantText {
                content: text.clone(),
                status: TranscriptBlockStatusV1::Completed,
            },
            TranscriptBlockBodyV1::Reasoning {
                request_id: "request".into(),
                content: text.clone(),
                status: TranscriptBlockStatusV1::Running,
            },
            TranscriptBlockBodyV1::Tool {
                call_id: "call".into(),
                tool_name: "read".into(),
                status: TranscriptBlockStatusV1::Queued,
                summary: Some("read".into()),
                summary_ref: None,
                output_ref: Some(reference.clone()),
            },
            TranscriptBlockBodyV1::Tool {
                call_id: "call".into(),
                tool_name: "read".into(),
                status: TranscriptBlockStatusV1::Failed,
                summary: None,
                summary_ref: Some(reference.clone()),
                output_ref: None,
            },
            TranscriptBlockBodyV1::Notice {
                notice_type: "info".into(),
                content: TranscriptTextContentV1::referenced(reference),
                status: TranscriptBlockStatusV1::Interrupted,
            },
        ];
        let blocks: Vec<_> = bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| TranscriptBlockV1 {
                block_id: format!("block-{index}"),
                block_revision: "1".into(),
                order_key: TranscriptOrderKeyV1 {
                    source_sequence: "1".into(),
                    ordinal: index as u32,
                },
                body,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&blocks).unwrap());
        return;
    }
    let schema = schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<TranscriptBlockV1>();
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}
