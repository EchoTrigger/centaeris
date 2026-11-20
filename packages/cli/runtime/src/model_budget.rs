//! Process-scoped request budgets, independent of catalog capacity and persistence.

pub(crate) fn from_env(context_capacity: u32, output_capacity: u32) -> Result<(u32, u32), String> {
    fn read(name: &str) -> Result<Option<String>, String> {
        match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} must be Unicode")),
        }
    }
    let context = read("CENTAERIS_MODEL_CONTEXT_BUDGET_TOKENS")?;
    let output = read("CENTAERIS_MODEL_OUTPUT_BUDGET_TOKENS")?;
    resolve(
        context_capacity,
        output_capacity,
        context.as_deref(),
        output.as_deref(),
    )
}

fn resolve(
    context_capacity: u32,
    output_capacity: u32,
    context: Option<&str>,
    output: Option<&str>,
) -> Result<(u32, u32), String> {
    if context.is_none() && output.is_none() {
        return Ok((context_capacity, output_capacity));
    }
    fn parse(value: Option<&str>, capacity: u32, label: &str) -> Result<u32, String> {
        let value = match value {
            None => capacity,
            Some(raw) => raw
                .parse::<u32>()
                .map_err(|_| format!("{label} must be an unsigned token count"))?,
        };
        if value == 0 || value > capacity {
            return Err(format!(
                "{label} must be positive and at most the catalog capacity {capacity}"
            ));
        }
        Ok(value)
    }
    let context = parse(context, context_capacity, "context budget")?;
    let output = parse(output, output_capacity, "output budget")?;
    if output >= context {
        return Err("output budget must be smaller than total context budget".into());
    }
    Ok((context, output))
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn model_budget_defaults_preserve_catalog_capacity() {
        assert_eq!(
            resolve(1_000_000, 384_000, None, None).unwrap(),
            (1_000_000, 384_000)
        );
        assert_eq!(
            resolve(32_000, 32_000, None, None).unwrap(),
            (32_000, 32_000)
        );
    }

    #[test]
    fn model_budget_total_and_output_are_independent() {
        assert_eq!(
            resolve(1_000_000, 384_000, Some("500000"), Some("64000")).unwrap(),
            (500_000, 64_000)
        );
    }

    #[test]
    fn model_budget_rejects_invalid_or_excessive_limits() {
        for (context, output) in [
            ("0", "1"),
            ("500k", "1"),
            ("1000001", "1"),
            ("500000", "500000"),
            ("500000", "384001"),
        ] {
            assert!(resolve(1_000_000, 384_000, Some(context), Some(output)).is_err());
        }
    }
}
