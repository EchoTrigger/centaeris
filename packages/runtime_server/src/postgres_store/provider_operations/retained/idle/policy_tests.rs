use super::*;
fn version(epoch: i64, predecessor: Option<&str>) -> IdleEnvironmentVersion {
    IdleEnvironmentVersion {
        environment_id: "environment".into(),
        controller_epoch: epoch,
        predecessor_operation_id: predecessor.map(str::to_owned),
    }
}
#[test]
fn idle_pause_waits_a_full_continuous_grace_and_has_an_exact_deadline() {
    let mut policy = IdlePausePolicy::new(Duration::from_millis(100)).unwrap();
    assert!(!policy.observe(version(2, None), 1_000).unwrap());
    assert!(!policy.observe(version(2, None), 1_099).unwrap());
    assert!(policy.observe(version(2, None), 1_100).unwrap());
}
#[test]
fn idle_pause_resets_on_activity_new_controller_power_tail_and_restart() {
    let mut policy = IdlePausePolicy::new(Duration::from_millis(100)).unwrap();
    assert!(!policy.observe(version(2, None), 1_000).unwrap());
    policy.forget("environment");
    assert!(!policy.observe(version(2, None), 1_100).unwrap());
    assert!(!policy.observe(version(3, None), 1_200).unwrap());
    assert!(!policy.observe(version(3, Some("resume")), 1_300).unwrap());
    assert!(!policy.observe(version(3, Some("resume")), 1_399).unwrap());
    assert!(policy.observe(version(3, Some("resume")), 1_400).unwrap());
    let mut restarted = IdlePausePolicy::new(Duration::from_millis(100)).unwrap();
    assert!(!restarted
        .observe(version(3, Some("resume")), 1_400)
        .unwrap());
}
#[test]
fn idle_pause_clock_rollback_restarts_grace_and_invalid_time_cannot_dispatch() {
    let mut policy = IdlePausePolicy::new(Duration::from_millis(100)).unwrap();
    assert!(!policy.observe(version(2, None), 1_000).unwrap());
    assert!(!policy.observe(version(2, None), 900).unwrap());
    assert!(!policy.observe(version(2, None), 999).unwrap());
    assert!(policy.observe(version(2, None), 1_000).unwrap());
    assert!(policy.observe(version(2, None), -1).is_err());
    assert!(IdlePausePolicy::new(Duration::ZERO).is_err());
    assert!(IdlePausePolicy::new(Duration::MAX).is_err());
}
#[test]
fn idle_pause_prunes_disappeared_targets_and_bounds_tracking_memory() {
    let mut policy = IdlePausePolicy::new(Duration::from_millis(100)).unwrap();
    policy.observe(version(2, None), 1_000).unwrap();
    policy.finish_scan();
    policy.finish_scan();
    assert!(policy.observations.is_empty());
    for index in 0..MAX_IDLE_OBSERVATIONS + 1 {
        let mut target = version(2, None);
        target.environment_id = format!("environment-{index}");
        assert!(!policy.observe(target, 1_000).unwrap());
    }
    assert_eq!(policy.observations.len(), MAX_IDLE_OBSERVATIONS);
    assert!(policy
        .observe(
            IdleEnvironmentVersion {
                environment_id: "environment-0".into(),
                ..version(2, None)
            },
            1_100
        )
        .unwrap());
}

#[test]
fn idle_pause_rollback_after_progress_starts_a_fresh_grace() {
    let mut policy = IdlePausePolicy::new(Duration::from_millis(100)).unwrap();
    assert!(!policy.observe(version(2, None), 1_000).unwrap());
    assert!(!policy.observe(version(2, None), 1_090).unwrap());
    assert!(!policy.observe(version(2, None), 1_050).unwrap());
    assert!(!policy.observe(version(2, None), 1_149).unwrap());
    assert!(policy.observe(version(2, None), 1_150).unwrap());
}
