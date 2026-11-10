use centaeris_core::source::SourceLineRangeV1;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CitationLocator {
    TextSpan {
        page_start: Option<u32>,
        page_end: Option<u32>,
        start_byte: u64,
        end_byte: u64,
        start_line: u32,
        end_line: u32,
    },
    PageRegion {
        page: u32,
        bbox: [u16; 4],
    },
    TableCell {
        page: u32,
        table_id: String,
        start_row: u32,
        end_row: u32,
        start_column: u32,
        end_column: u32,
    },
}

impl CitationLocator {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::TextSpan {
                page_start,
                page_end,
                start_byte,
                end_byte,
                start_line,
                end_line,
            } => {
                let valid_pages = match (page_start, page_end) {
                    (None, None) => true,
                    (Some(start), Some(end)) => *start > 0 && start <= end,
                    _ => false,
                };
                if !valid_pages
                    || start_byte >= end_byte
                    || *start_line == 0
                    || start_line > end_line
                {
                    return Err("invalid textSpan locator".to_string());
                }
            }
            Self::PageRegion { page, bbox } => {
                if *page == 0 {
                    return Err("pageRegion page must be positive".to_string());
                }
                validate_bbox(*bbox)?;
            }
            Self::TableCell {
                page,
                table_id,
                start_row,
                end_row,
                start_column,
                end_column,
            } => {
                if *page == 0
                    || *start_row > *end_row
                    || *start_column > *end_column
                    || table_id.trim().is_empty()
                {
                    return Err("invalid tableCell locator".to_string());
                }
            }
        }
        Ok(())
    }
}

fn validate_bbox([left, top, right, bottom]: [u16; 4]) -> Result<(), String> {
    if right > 10_000 || bottom > 10_000 || left >= right || top >= bottom {
        return Err("bbox must be a non-empty 0-10000 rectangle".to_string());
    }
    Ok(())
}

pub(super) fn line_range(value: &Value) -> Result<Option<SourceLineRangeV1>, String> {
    if value.get("kind").is_some() {
        let locator: CitationLocator = serde_json::from_value(value.clone())
            .map_err(|error| format!("stored citation locator is invalid: {error}"))?;
        locator.validate()?;
        return Ok(match locator {
            CitationLocator::TextSpan {
                start_line,
                end_line,
                ..
            } => Some(SourceLineRangeV1 {
                start_line: u64::from(start_line),
                end_line: u64::from(end_line),
            }),
            CitationLocator::PageRegion { .. } | CitationLocator::TableCell { .. } => None,
        });
    }
    let locator = value
        .as_object()
        .ok_or("stored citation locator must be an object")?;
    if locator.len() != 2 {
        return Err("stored citation locator fields are invalid".into());
    }
    let (start, end, lines) =
        if locator.contains_key("startLine") && locator.contains_key("endLine") {
            (&locator["startLine"], &locator["endLine"], true)
        } else if locator.contains_key("pageStart") && locator.contains_key("pageEnd") {
            (&locator["pageStart"], &locator["pageEnd"], false)
        } else {
            return Err("stored citation locator fields are invalid".into());
        };
    let (Some(start), Some(end)) = (start.as_u64(), end.as_u64()) else {
        return Err("stored citation locator range is invalid".into());
    };
    if start == 0 || start > end {
        return Err("stored citation locator range is invalid".into());
    }
    Ok(lines.then_some(SourceLineRangeV1 {
        start_line: start,
        end_line: end,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_locator_values_fail_loudly() {
        let invalid = CitationLocator::PageRegion {
            page: 1,
            bbox: [0, 0, 10_001, 100],
        };
        assert!(invalid.validate().is_err());

        let locator = serde_json::from_value::<CitationLocator>(serde_json::json!({
            "kind": "textSpan",
            "pageStart": 2,
            "pageEnd": 4,
            "startByte": 10,
            "endByte": 30,
            "startLine": 4,
            "endLine": 8
        }))
        .expect("multi-page textSpan locator");
        locator.validate().expect("valid multi-page locator");
        assert!(
            serde_json::from_value::<CitationLocator>(serde_json::json!({
                "kind": "textSpan",
                "page": 2,
                "startByte": 10,
                "endByte": 30,
                "startLine": 4,
                "endLine": 8
            }))
            .is_err()
        );
    }
}
