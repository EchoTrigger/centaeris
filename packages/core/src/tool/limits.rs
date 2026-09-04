use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::Serialize;

pub const MAX_TOOL_DESCRIPTION_CHARS: usize = 4096;
pub const MAX_TOOL_DESCRIPTION_BYTES: usize = 16 * 1024;
pub const MAX_TOOL_INPUT_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_TOOL_CONTRACT_BYTES: usize = 4 * 1024 * 1024;

pub fn validate_tool_description(value: &str) -> Result<(), String> {
    if value.len() > MAX_TOOL_DESCRIPTION_BYTES
        || value.chars().take(MAX_TOOL_DESCRIPTION_CHARS + 1).count() > MAX_TOOL_DESCRIPTION_CHARS
        || value.trim().is_empty()
    {
        return Err(
            "tool description must be nonblank and at most 4096 characters / 16384 UTF-8 bytes"
                .to_string(),
        );
    }
    Ok(())
}

pub fn json_size_with_limit<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<usize, String> {
    struct Counter {
        size: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit - self.size {
                return Err(io::Error::other("JSON byte limit exceeded"));
            }
            self.size += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { size: 0, limit };
    serde_json::to_writer(&mut counter, value)
        .map_err(|error| format!("JSON must fit within {limit} bytes: {error}"))?;
    Ok(counter.size)
}

#[derive(Debug, Clone)]
pub struct ToolContractBudget {
    bytes: usize,
    empty: bool,
}

impl Default for ToolContractBudget {
    fn default() -> Self {
        Self {
            bytes: 2,
            empty: true,
        }
    }
}

impl ToolContractBudget {
    pub fn add<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), String> {
        let separator = usize::from(!self.empty);
        let remaining = MAX_TOOL_CONTRACT_BYTES
            .checked_sub(self.bytes + separator)
            .ok_or_else(|| "tool contracts exceed 4194304 bytes".to_string())?;
        let size = json_size_with_limit(value, remaining)
            .map_err(|error| format!("tool contracts exceed 4194304 bytes: {error}"))?;
        self.bytes += separator + size;
        self.empty = false;
        Ok(())
    }
}

pub fn read_tool_contract_file(path: &Path) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|error| format!("open tool contract file {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_TOOL_CONTRACT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read tool contract file {}: {error}", path.display()))?;
    if bytes.len() > MAX_TOOL_CONTRACT_BYTES {
        return Err(format!(
            "tool contract file {} exceeds 4194304 bytes",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        format!(
            "tool contract file {} must be UTF-8: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_preserve_unicode_and_enforce_exact_limits() {
        for description in [
            "a".repeat(4096),
            "界".repeat(4096),
            "🦀".repeat(4096),
            "\n  Text.\t".to_string(),
        ] {
            validate_tool_description(&description).unwrap();
        }
        for description in [
            "a".repeat(4097),
            "界".repeat(4097),
            "🦀".repeat(4097),
            " \n\t".to_string(),
        ] {
            assert!(validate_tool_description(&description).is_err());
        }
    }

    #[test]
    fn bounded_json_counts_escaping_and_array_overhead_without_mutating_on_failure() {
        assert_eq!(json_size_with_limit("\n🦀", 8).unwrap(), 8);
        assert!(json_size_with_limit("\n🦀", 7).is_err());
        let schema = serde_json::json!({"x": "a".repeat(MAX_TOOL_INPUT_SCHEMA_BYTES - 8)});
        assert_eq!(
            json_size_with_limit(&schema, MAX_TOOL_INPUT_SCHEMA_BYTES).unwrap(),
            MAX_TOOL_INPUT_SCHEMA_BYTES
        );
        assert!(json_size_with_limit(&schema, MAX_TOOL_INPUT_SCHEMA_BYTES - 1).is_err());
        let mut budget = ToolContractBudget::default();
        budget
            .add(&"a".repeat(MAX_TOOL_CONTRACT_BYTES - 7))
            .unwrap();
        assert!(budget.add(&"xx").is_err());
        budget.add("").unwrap();
        assert!(budget.add("").is_err());
    }

    #[test]
    fn bounded_file_read_rejects_large_raw_files_before_parsing() {
        let path =
            std::env::temp_dir().join(format!("centaeris-contract-limit-{}", std::process::id()));
        let file = File::create(&path).unwrap();
        file.set_len((MAX_TOOL_CONTRACT_BYTES + 1) as u64).unwrap();
        drop(file);
        let result = read_tool_contract_file(&path);
        std::fs::remove_file(path).unwrap();
        assert!(result.unwrap_err().contains("exceeds 4194304 bytes"));
    }
}
