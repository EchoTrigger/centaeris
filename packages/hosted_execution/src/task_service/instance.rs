//! Select only the container cgroup which demonstrably contains this process.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn task_domain(
    parent: &Path,
    pid: u32,
    verify: impl Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    use std::io::Read;
    const ERROR: &str = "task_instance_cgroup_unavailable";
    verify(parent)?;
    let mut selected = None;
    let entries = fs::read_dir(parent).map_err(|_| ERROR)?;
    for (index, entry) in entries.enumerate() {
        if index >= 128 {
            return Err(ERROR.into());
        }
        let entry = entry.map_err(|_| ERROR)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(ERROR.into());
        };
        let Some(id) = name
            .strip_prefix("docker-")
            .and_then(|s| s.strip_suffix(".scope"))
        else {
            continue;
        };
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !entry.file_type().map_err(|_| ERROR)?.is_dir()
            || selected.is_some()
        {
            return Err(ERROR.into());
        }
        let path = entry.path();
        verify(&path)?;
        let mut pids = String::new();
        fs::File::open(path.join("cgroup.procs"))
            .map_err(|_| ERROR)?
            .take(128 * 1024 + 1)
            .read_to_string(&mut pids)
            .map_err(|_| ERROR)?;
        if pids.len() > 128 * 1024 {
            return Err(ERROR.into());
        }
        let pids = pids
            .lines()
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ERROR)?;
        if !pids.contains(&pid) {
            return Err(ERROR.into());
        }
        selected = Some(path);
    }
    let tasks = selected.ok_or(ERROR)?.join("tasks");
    // Existing tasks mean an earlier initialization may already have run.
    fs::create_dir(&tasks).map_err(|_| ERROR)?;
    verify(&tasks)?;
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "centaeris-instance-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn domain(root: &Path, digit: char, pids: &str) -> PathBuf {
        let path = root.join(format!("docker-{}.scope", digit.to_string().repeat(64)));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("cgroup.procs"), pids).unwrap();
        path
    }

    #[test]
    fn instance_task_domain_requires_unique_positive_process_membership() {
        let root = temporary_root();
        assert!(task_domain(root.as_path(), 1, |_| Ok(())).is_err());
        let own = domain(root.as_path(), 'a', "1\n17\n");
        assert_eq!(
            task_domain(root.as_path(), 1, |_| Ok(())).unwrap(),
            own.join("tasks")
        );
        assert!(own.join("tasks").is_dir());
        // A second initialization cannot silently adopt existing task state.
        assert!(task_domain(root.as_path(), 1, |_| Ok(())).is_err());
    }

    #[test]
    fn instance_task_domain_refuses_foreign_or_ambiguous_or_unverified_domains() {
        for pids in ["", "11\n", "1x\n"] {
            let root = temporary_root();
            let path = domain(root.as_path(), 'a', pids);
            assert!(task_domain(root.as_path(), 1, |_| Ok(())).is_err());
            assert!(!path.join("tasks").exists());
        }
        let root = temporary_root();
        let own = domain(root.as_path(), 'a', "1\n");
        domain(root.as_path(), 'b', "1\n");
        assert!(task_domain(root.as_path(), 1, |_| Ok(())).is_err());
        assert!(!own.join("tasks").exists());
        let root = temporary_root();
        let own = domain(root.as_path(), 'a', "1\n");
        assert!(task_domain(root.as_path(), 1, |_| Err("wrong filesystem".into())).is_err());
        assert!(!own.join("tasks").exists());
    }
}
