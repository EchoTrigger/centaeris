use centaeris_core::session::transcript::{
    validate_transcript_resume_cursor_read, TranscriptResumeCursorV1,
    TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT,
};

#[test]
fn resume_cursor_read_requires_one_sorted_bounded_cursor_per_stream() {
    let valid = vec![
        TranscriptResumeCursorV1 {
            stream_id: "run-a".to_string(),
            cursor: "cursor-a".to_string(),
        },
        TranscriptResumeCursorV1 {
            stream_id: "run-b".to_string(),
            cursor: "cursor-b".to_string(),
        },
    ];
    assert!(validate_transcript_resume_cursor_read(&valid).is_ok());

    let duplicate = vec![
        TranscriptResumeCursorV1 {
            stream_id: "run-a".to_string(),
            cursor: "cursor-a".to_string(),
        },
        TranscriptResumeCursorV1 {
            stream_id: "run-a".to_string(),
            cursor: "cursor-newer".to_string(),
        },
    ];
    assert!(validate_transcript_resume_cursor_read(&duplicate).is_err());

    let unsorted = vec![
        TranscriptResumeCursorV1 {
            stream_id: "run-b".to_string(),
            cursor: "cursor-b".to_string(),
        },
        TranscriptResumeCursorV1 {
            stream_id: "run-a".to_string(),
            cursor: "cursor-a".to_string(),
        },
    ];
    assert!(validate_transcript_resume_cursor_read(&unsorted).is_err());

    let oversized = (0..=TRANSCRIPT_PAGE_RESUME_CURSOR_MAX_COUNT)
        .map(|index| TranscriptResumeCursorV1 {
            stream_id: format!("run-{index:03}"),
            cursor: format!("cursor-{index:03}"),
        })
        .collect::<Vec<_>>();
    assert!(validate_transcript_resume_cursor_read(&oversized).is_err());
}
