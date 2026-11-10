use super::{CpuCounters, MemoryEvents, Pressure, PressureLine};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, PartialEq, Eq)]
pub(in crate::host_supervisor) struct HostMemory {
    pub total: u64,
    pub available: u64,
}
pub(in crate::host_supervisor) fn scalar(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("host_resource_counter_invalid".into());
    }
    text.parse()
        .map_err(|_| "host_resource_counter_invalid".into())
}
pub(in crate::host_supervisor) fn memory(text: &str) -> Result<HostMemory, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !matches!(key, "MemTotal" | "MemAvailable") {
            continue;
        }
        let fields = value.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || fields[1] != "kB" || values.contains_key(key) {
            return Err("host_resource_memory_invalid".into());
        }
        values.insert(
            key,
            scalar(fields[0])?
                .checked_mul(1024)
                .ok_or("host_resource_memory_overflow")?,
        );
    }
    let total = *values
        .get("MemTotal")
        .ok_or("host_resource_memory_unavailable")?;
    let available = *values
        .get("MemAvailable")
        .ok_or("host_resource_memory_unavailable")?;
    if total == 0 || available > total {
        return Err("host_resource_memory_invalid".into());
    }
    Ok(HostMemory { total, available })
}
fn counters(text: &str) -> Result<BTreeMap<&str, u64>, String> {
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || result.insert(fields[0], scalar(fields[1])?).is_some() {
            return Err("host_resource_counter_invalid".into());
        }
    }
    Ok(result)
}
fn required(map: &BTreeMap<&str, u64>, name: &str) -> Result<u64, String> {
    map.get(name)
        .copied()
        .ok_or_else(|| "host_resource_counter_unavailable".into())
}
pub(in crate::host_supervisor) fn cpu(text: &str) -> Result<CpuCounters, String> {
    let m = counters(text)?;
    Ok(CpuCounters {
        usage_usec: required(&m, "usage_usec")?,
        user_usec: required(&m, "user_usec")?,
        system_usec: required(&m, "system_usec")?,
        nr_periods: m.get("nr_periods").copied(),
        nr_throttled: m.get("nr_throttled").copied(),
        throttled_usec: m.get("throttled_usec").copied(),
    })
}
pub(in crate::host_supervisor) fn memory_events(text: &str) -> Result<MemoryEvents, String> {
    let m = counters(text)?;
    Ok(MemoryEvents {
        low: required(&m, "low")?,
        high: required(&m, "high")?,
        max: required(&m, "max")?,
        oom: required(&m, "oom")?,
        oom_kill: required(&m, "oom_kill")?,
        oom_group_kill: m.get("oom_group_kill").copied(),
    })
}
pub(in crate::host_supervisor) fn pressure(text: &str) -> Result<Pressure, String> {
    let mut lines = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let name = fields.next().ok_or("host_resource_pressure_invalid")?;
        if !matches!(name, "some" | "full") || lines.contains_key(name) {
            return Err("host_resource_pressure_invalid".into());
        }
        let mut values = BTreeMap::new();
        for field in fields {
            let (key, value) = field
                .split_once('=')
                .ok_or("host_resource_pressure_invalid")?;
            if values.insert(key, value).is_some() {
                return Err("host_resource_pressure_invalid".into());
            }
        }
        if values.len() != 4 {
            return Err("host_resource_pressure_invalid".into());
        }
        let number = |key| -> Result<f64, String> {
            let value = values
                .get(key)
                .ok_or("host_resource_pressure_invalid")?
                .parse::<f64>()
                .map_err(|_| "host_resource_pressure_invalid")?;
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err("host_resource_pressure_invalid".into());
            }
            Ok(value)
        };
        lines.insert(
            name,
            PressureLine {
                avg10: number("avg10")?,
                avg60: number("avg60")?,
                avg300: number("avg300")?,
                total_micros: scalar(
                    values
                        .get("total")
                        .ok_or("host_resource_pressure_invalid")?,
                )?,
            },
        );
    }
    Ok(Pressure {
        some: lines
            .remove("some")
            .ok_or("host_resource_pressure_unavailable")?,
        full: lines.remove("full"),
    })
}
pub(in crate::host_supervisor) fn uptime_ms(text: &str) -> Result<u64, String> {
    let first = text
        .split_whitespace()
        .next()
        .ok_or("host_resource_time_invalid")?;
    let (whole, fraction) = first.split_once('.').ok_or("host_resource_time_invalid")?;
    if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err("host_resource_time_invalid".into());
    }
    let mut millis = fraction.chars().take(3).collect::<String>();
    while millis.len() < 3 {
        millis.push('0');
    }
    scalar(whole)?
        .checked_mul(1000)
        .and_then(|v| v.checked_add(millis.parse::<u64>().ok()?))
        .ok_or_else(|| "host_resource_time_invalid".into())
}
pub(in crate::host_supervisor) fn cpu_count(text: &str) -> Result<u32, String> {
    let mut cpus = BTreeSet::new();
    for line in text.lines() {
        let name = line.split_whitespace().next().unwrap_or("");
        if let Some(id) = name.strip_prefix("cpu").filter(|v| !v.is_empty()) {
            if !id.bytes().all(|b| b.is_ascii_digit()) || !cpus.insert(scalar(id)?) {
                return Err("host_resource_cpu_count_invalid".into());
            }
        }
    }
    u32::try_from(cpus.len())
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| "host_resource_cpu_count_unavailable".into())
}
