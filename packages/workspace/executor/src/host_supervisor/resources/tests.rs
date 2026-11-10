use super::*;
fn target() -> HostLaunchTarget {
    HostLaunchTarget {
        environment_instance_id: format!("instance_{}", "a".repeat(64)),
        container_id: "b".repeat(64),
        environment_id: "environment-a".into(),
        profile_digest: format!("sha256:{}", "c".repeat(64)),
        image_digest: format!("sha256:{}", "d".repeat(64)),
    }
}
fn pressure() -> Pressure {
    Pressure {
        some: PressureLine {
            avg10: 0.0,
            avg60: 0.0,
            avg300: 0.0,
            total_micros: 0,
        },
        full: None,
    }
}
fn snapshot() -> HostResourceObservation {
    HostResourceObservation {
        schema: HOST_RESOURCE_SCHEMA.into(),
        host_id: "daemon-a".into(),
        boot_id: "12345678-1234-1234-1234-123456789abc".into(),
        observed_at_boot_ms: 1000,
        collection_duration_ms: 10,
        mem_total_bytes: 4096,
        mem_available_bytes: 2048,
        cpu_count: 2,
        memory_pressure: pressure(),
        cpu_pressure: pressure(),
        containers: vec![ContainerResourceObservation::Unknown {
            target: target(),
            reason: "cgroup_unavailable".into(),
        }],
    }
}
#[test]
fn request_accepts_empty_inventory_for_first_admission_and_valid_bound_targets() {
    assert!(validate_resource_request("daemon-a", &[]).is_ok());
    assert!(validate_resource_request("daemon-a", &[target()]).is_ok());
}
#[test]
fn request_rejects_duplicate_instance_or_container_without_silently_deduplicating() {
    let first = target();
    let mut other = first.clone();
    other.environment_instance_id = format!("instance_{}", "e".repeat(64));
    other.environment_id = "other".into();
    assert!(validate_resource_request("daemon-a", &[first.clone(), other]).is_err());
    let mut other = first.clone();
    other.container_id = "f".repeat(64);
    assert!(validate_resource_request("daemon-a", &[first, other]).is_err());
}
#[test]
fn request_rejects_unbounded_or_invalid_targets() {
    assert!(validate_resource_request("", &[]).is_err());
    assert!(
        validate_resource_request("daemon-a", &vec![target(); MAX_RESOURCE_TARGETS + 1]).is_err()
    );
    let mut bad = target();
    bad.container_id = "../other".into();
    assert!(validate_resource_request("daemon-a", &[bad]).is_err());
}
#[test]
fn valid_unknown_container_stays_explicitly_unknown_and_is_not_zero_usage() {
    let value = snapshot();
    let clock = ObservationClock {
        boot_id: value.boot_id.clone(),
        now_boot_ms: 1100,
        max_age_ms: 200,
    };
    assert!(value.validate_for("daemon-a", &[target()], &clock).is_ok());
    let wire = serde_json::to_value(&value).unwrap();
    assert_eq!(wire["containers"][0]["state"], "unknown");
    assert!(wire["containers"][0].get("memoryCurrentBytes").is_none());
}
#[test]
fn observation_rejects_stale_future_or_restarted_host_and_missing_targets() {
    let value = snapshot();
    let clock = ObservationClock {
        boot_id: value.boot_id.clone(),
        now_boot_ms: 1500,
        max_age_ms: 200,
    };
    assert!(value.validate_for("daemon-a", &[target()], &clock).is_err());
    let mut clock = clock;
    clock.now_boot_ms = 900;
    assert!(value.validate_for("daemon-a", &[target()], &clock).is_err());
    clock.now_boot_ms = 1100;
    clock.boot_id = "new-boot".into();
    assert!(value.validate_for("daemon-a", &[target()], &clock).is_err());
    clock.boot_id = value.boot_id.clone();
    assert!(value
        .validate_for("wrong-daemon", &[target()], &clock)
        .is_err());
    let mut value = value;
    value.containers.clear();
    assert!(value.validate_for("daemon-a", &[target()], &clock).is_err());
}
#[test]
fn protocol_rejects_extra_fields_instead_of_accepting_another_measurement_shape() {
    let mut value = serde_json::to_value(snapshot()).unwrap();
    value["workingSetBytes"] = 42.into();
    assert!(serde_json::from_value::<HostResourceObservation>(value).is_err());
}
#[test]
fn host_memory_uses_memavailable_not_memfree_and_checks_units_and_duplicates() {
    let got =
        parse::memory("MemTotal: 4096 kB\nMemFree: 1 kB\nMemAvailable: 1024 kB\nCached: 2000 kB\n")
            .unwrap();
    assert_eq!((got.total, got.available), (4194304, 1048576));
    for invalid in [
        "MemTotal: 4096 kB\nMemFree: 10 kB",
        "MemTotal: 4 bytes\nMemAvailable: 1 kB",
        "MemTotal: 4 kB\nMemAvailable: 5 kB",
        "MemTotal: 4 kB\nMemAvailable: 1 kB\nMemAvailable: 2 kB",
    ] {
        assert!(parse::memory(invalid).is_err(), "{invalid}");
    }
}
#[test]
fn cgroup_current_is_exact_including_cache_and_tmpfs_is_a_separate_measurement() {
    assert_eq!(parse::scalar("12345678\n").unwrap(), 12345678);
    for invalid in ["max", "-1", "", "1 2", "18446744073709551616"] {
        assert!(parse::scalar(invalid).is_err());
    }
    let value = MeasuredBytes::Unknown {
        reason: "guest_tmpfs_not_observed".into(),
    };
    assert_eq!(serde_json::to_value(value).unwrap()["state"], "unknown");
}
#[test]
fn pressure_retains_some_full_and_total_without_fabricating_optional_cpu_full() {
    let got=parse::pressure("some avg10=1.25 avg60=0.50 avg300=0.00 total=1234\nfull avg10=0.25 avg60=0.00 avg300=0.00 total=22\n").unwrap();
    assert_eq!(got.some.total_micros, 1234);
    assert_eq!(got.full.unwrap().avg10, 0.25);
    assert!(parse::pressure("some avg10=0 avg60=0 avg300=0 total=0")
        .unwrap()
        .full
        .is_none());
    for invalid in [
        "",
        "some avg10=NaN avg60=0 avg300=0 total=0",
        "some avg10=101 avg60=0 avg300=0 total=0",
        "some avg10=1 avg60=2 total=3",
    ] {
        assert!(parse::pressure(invalid).is_err());
    }
}
#[test]
fn cpu_and_memory_events_preserve_cumulative_counters_and_require_observed_fields() {
    let got=parse::cpu("usage_usec 123\nuser_usec 70\nsystem_usec 53\nnr_periods 8\nnr_throttled 2\nthrottled_usec 5\n").unwrap();
    assert_eq!((got.usage_usec, got.nr_throttled), (123, Some(2)));
    assert!(parse::cpu("usage_usec 123").is_err());
    let got = parse::memory_events("low 0\nhigh 2\nmax 1\noom 1\noom_kill 1\n").unwrap();
    assert_eq!(got.oom_kill, 1);
    assert_eq!(got.oom_group_kill, None);
    assert!(parse::memory_events("high 0\nmax 0").is_err());
}
#[test]
fn monotonic_timestamp_and_host_cpu_count_are_observed_not_profile_caps() {
    assert_eq!(parse::uptime_ms("123.45 999.00\n").unwrap(), 123450);
    assert_eq!(
        parse::cpu_count("cpu 1 2 3\ncpu0 1 2 3\ncpu1 1 2 3\nintr 1\n").unwrap(),
        2
    );
    assert!(parse::cpu_count("cpu 1 2 3").is_err());
    assert!(parse::uptime_ms("NaN 0").is_err());
}

#[test]
fn resource_observation_request_reuses_strict_supervisor_envelope() {
    let wire = serde_json::json!({"schema":super::super::HOST_SUPERVISOR_SCHEMA,"request":{"operation":"observeResources","expectedHostId":"daemon-a","targets":[]}});
    let request = serde_json::from_value::<super::super::HostSupervisorEnvelope>(wire)
        .expect("resource request is a hosted-only control operation");
    request.validate().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn observation_clock_reads_kernel_boot_and_monotonic_time() {
    let before = ObservationClock::from_host_kernel(2500).unwrap();
    let after = ObservationClock::from_host_kernel(2500).unwrap();
    assert_eq!(before.boot_id, after.boot_id);
    assert!(valid_boot_id(&before.boot_id));
    assert!(after.now_boot_ms >= before.now_boot_ms);
    assert_eq!(after.max_age_ms, 2500);
    assert!(ObservationClock::from_host_kernel(0).is_err());
}
