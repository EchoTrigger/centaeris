use super::*;
use crate::host_supervisor::{resources::MeasuredBytes, HostLaunchTarget};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    roots: ProbeRoots,
    reg: Registration,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "centaeris-resource-fixture-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        let roots = ProbeRoots {
            proc: root.join("proc"),
            cgroup: root.join("cgroup"),
        };
        fs::create_dir_all(roots.proc.join("100")).unwrap();
        let group = roots
            .cgroup
            .join(format!("docker-{}.scope", "b".repeat(64)));
        fs::create_dir_all(&group).unwrap();
        let target = HostLaunchTarget {
            environment_instance_id: format!("instance_{}", "a".repeat(64)),
            container_id: "b".repeat(64),
            environment_id: "environment-a".into(),
            profile_digest: format!("sha256:{}", "c".repeat(64)),
            image_digest: format!("sha256:{}", "d".repeat(64)),
        };
        let reg = Registration {
            target,
            init_pid: 100,
            init_start_time: 12,
            bundle: "unused".into(),
            cgroup: group.to_str().unwrap().into(),
            spec_digest: "unused".into(),
            process: serde_json::json!({}),
        };
        let result = Self { root, roots, reg };
        result.write_pid(12);
        result.write_membership();
        result.write_metrics();
        result
    }
    fn write_pid(&self, start: u64) {
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[19] = start.to_string();
        fs::write(
            self.roots.proc.join("100/stat"),
            format!("100 (keeper) {}", fields.join(" ")),
        )
        .unwrap();
    }
    fn write_membership(&self) {
        fs::write(
            self.roots.proc.join("100/cgroup"),
            format!("0::/docker-{}.scope\n", self.reg.target.container_id),
        )
        .unwrap();
    }
    fn write_metrics(&self) {
        for (name,text) in [("memory.current","12345678\n"),("memory.swap.current","2097152\n"),("pids.current","5\n"),("cgroup.procs","100\n200\n"),("cpu.stat","usage_usec 123\nuser_usec 70\nsystem_usec 53\n"),("memory.events","low 0\nhigh 2\nmax 0\noom 0\noom_kill 0\n"),("memory.pressure","some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"),("cpu.pressure","some avg10=1.00 avg60=0.00 avg300=0.00 total=50\n")] {fs::write(PathBuf::from(&self.reg.cgroup).join(name),text).unwrap();}
    }
    fn sample(&self) -> ContainerResourceObservation {
        sample_target(&self.reg, &self.roots, || Ok(self.reg.clone()))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn unknown(value: ContainerResourceObservation) {
    assert!(matches!(
        value,
        ContainerResourceObservation::Unknown { .. }
    ));
}
#[test]
fn complete_cgroup_sample_reads_actual_current_swap_pids_and_counters() {
    let f = Fixture::new();
    let ContainerResourceObservation::Observed { resources } = f.sample() else {
        panic!("expected actual measurement")
    };
    assert_eq!(resources.memory_current_bytes, 12345678);
    assert_eq!(resources.memory_swap_current_bytes, 2097152);
    assert_eq!(resources.pids_current, 5);
    assert_eq!(resources.cpu_counters.usage_usec, 123);
    assert_eq!(resources.memory_events.high, 2);
    assert!(matches!(
        resources.workspace_usage,
        MeasuredBytes::Unknown { .. }
    ));
}
#[test]
fn pid_reuse_or_cgroup_membership_change_is_unknown_after_sampling() {
    let f = Fixture::new();
    unknown(sample_target(&f.reg, &f.roots, || {
        f.write_pid(13);
        Ok(f.reg.clone())
    }));
    let f = Fixture::new();
    unknown(sample_target(&f.reg, &f.roots, || {
        fs::write(f.roots.proc.join("100/cgroup"), "0::/other\n").unwrap();
        Ok(f.reg.clone())
    }));
}
#[test]
fn physical_identity_confirmation_failure_is_not_zero_usage() {
    let f = Fixture::new();
    unknown(sample_target(&f.reg, &f.roots, || {
        Err("docker_unavailable".into())
    }));
    let f = Fixture::new();
    unknown(sample_target(&f.reg, &f.roots, || {
        let mut other = f.reg.clone();
        other.init_pid = 200;
        Ok(other)
    }));
}
#[test]
fn cgroup_directory_replacement_invalidates_a_collected_sample() {
    let f = Fixture::new();
    unknown(sample_target(&f.reg, &f.roots, || {
        let p = PathBuf::from(&f.reg.cgroup);
        fs::rename(&p, f.root.join("old-cgroup")).unwrap();
        fs::create_dir(&p).unwrap();
        f.write_metrics();
        Ok(f.reg.clone())
    }));
}
#[test]
fn missing_or_malformed_required_counter_returns_unknown() {
    let f = Fixture::new();
    fs::remove_file(PathBuf::from(&f.reg.cgroup).join("memory.swap.current")).unwrap();
    unknown(f.sample());
    let f = Fixture::new();
    fs::write(
        PathBuf::from(&f.reg.cgroup).join("memory.current"),
        "not-observed",
    )
    .unwrap();
    unknown(f.sample());
}
#[test]
fn escaped_or_foreign_cgroup_path_cannot_be_sampled() {
    let f = Fixture::new();
    let mut reg = f.reg.clone();
    reg.cgroup = f.root.to_str().unwrap().into();
    unknown(sample_target(&reg, &f.roots, || Ok(reg.clone())));
    let f = Fixture::new();
    let mut reg = f.reg.clone();
    reg.cgroup = format!(
        "{}/../cgroup/docker-{}.scope",
        f.roots.cgroup.display(),
        reg.target.container_id
    );
    unknown(sample_target(&reg, &f.roots, || Ok(reg.clone())));
}
#[test]
fn metric_and_cgroup_symlinks_are_not_followed() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let metric = PathBuf::from(&f.reg.cgroup).join("memory.current");
    fs::remove_file(&metric).unwrap();
    fs::write(f.root.join("outside"), "99").unwrap();
    symlink(f.root.join("outside"), metric).unwrap();
    unknown(f.sample());
    let f = Fixture::new();
    let group = PathBuf::from(&f.reg.cgroup);
    fs::rename(&group, f.root.join("other-group")).unwrap();
    symlink(f.root.join("other-group"), group).unwrap();
    unknown(f.sample());
}
#[test]
fn init_pid_must_be_present_in_the_exact_observed_cgroup() {
    let f = Fixture::new();
    fs::write(PathBuf::from(&f.reg.cgroup).join("cgroup.procs"), "200\n").unwrap();
    unknown(f.sample());
}

impl Fixture {
    fn write_host(&self) {
        fs::create_dir_all(self.roots.proc.join("sys/kernel/random")).unwrap();
        fs::create_dir_all(self.roots.proc.join("pressure")).unwrap();
        for(name,text)in[("meminfo","MemTotal: 4096 kB\nMemAvailable: 2048 kB\nMemFree: 1 kB\n"),("uptime","12.34 1.00\n"),("stat","cpu 1 2 3\ncpu0 1 2 3\ncpu1 1 2 3\n"),("sys/kernel/random/boot_id","12345678-1234-1234-1234-123456789abc\n"),("pressure/memory","some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"),("pressure/cpu","some avg10=1.00 avg60=0.00 avg300=0.00 total=50\n")]{fs::write(self.roots.proc.join(name),text).unwrap();}
    }
    fn daemon(&self) -> DaemonIdentity {
        DaemonIdentity {
            id: "daemon-a".into(),
            memory_bytes: 4194304,
            cpu_count: 2,
        }
    }
}
#[test]
fn first_host_sample_observes_available_ram_without_any_existing_environment() {
    let f = Fixture::new();
    f.write_host();
    let value = collect(
        ResourceRequest {
            host: "daemon-a",
            targets: &[],
        },
        &f.roots,
        |_| panic!("empty inventory must not attest a target"),
        || Ok(f.daemon()),
    )
    .unwrap();
    assert_eq!(value.mem_total_bytes, 4194304);
    assert_eq!(value.mem_available_bytes, 2097152);
    assert_eq!(value.cpu_count, 2);
    assert_eq!(value.observed_at_boot_ms, 12340);
    assert!(value.containers.is_empty());
}
#[test]
fn collection_preserves_unknown_target_instead_of_omitting_or_zero_charging_it() {
    let f = Fixture::new();
    f.write_host();
    let targets = [f.reg.target.clone()];
    let value = collect(
        ResourceRequest {
            host: "daemon-a",
            targets: &targets,
        },
        &f.roots,
        |_| Err("host_target_unknown".into()),
        || Ok(f.daemon()),
    )
    .unwrap();
    assert_eq!(value.containers.len(), 1);
    unknown(value.containers.into_iter().next().unwrap());
}
#[test]
fn collection_rejects_daemon_or_boot_change_during_a_sample() {
    let f = Fixture::new();
    f.write_host();
    let mut reads = 0;
    assert!(collect(
        ResourceRequest {
            host: "daemon-a",
            targets: &[]
        },
        &f.roots,
        |_| Ok(f.reg.clone()),
        || {
            reads += 1;
            let mut d = f.daemon();
            if reads > 1 {
                d.id = "daemon-b".into();
            }
            Ok(d)
        }
    )
    .is_err());
    let f = Fixture::new();
    f.write_host();
    let targets = [f.reg.target.clone()];
    assert!(collect(
        ResourceRequest {
            host: "daemon-a",
            targets: &targets
        },
        &f.roots,
        |_| {
            fs::write(
                f.roots.proc.join("sys/kernel/random/boot_id"),
                "22222222-1234-1234-1234-123456789abc\n",
            )
            .unwrap();
            Ok(f.reg.clone())
        },
        || Ok(f.daemon())
    )
    .is_err());
}
#[test]
fn missing_host_memavailable_is_unknown_not_memfree_fallback() {
    let f = Fixture::new();
    f.write_host();
    fs::write(
        f.roots.proc.join("meminfo"),
        "MemTotal: 4096 kB\nMemFree: 2048 kB\n",
    )
    .unwrap();
    assert!(collect(
        ResourceRequest {
            host: "daemon-a",
            targets: &[]
        },
        &f.roots,
        |_| Ok(f.reg.clone()),
        || Ok(f.daemon())
    )
    .is_err());
}
