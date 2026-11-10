use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

const SOURCE_ENV: &str = "CENTAERIS_SYSTEM_SKILLS_SOURCE";
const DIGEST_MARKER: &str = ".system-skills.sha256";
const STAGING_NAME: &str = ".system-skills.staging";
const BACKUP_NAME: &str = ".system-skills.backup";
const MAX_WARNING_CHARS: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
struct Deployment {
    digest: String,
    changed: bool,
}

pub(crate) fn deploy() {
    let target = crate::user_data_layout::system_skills_dir();
    let result = discover_source().and_then(|source| deploy_from(source.as_deref(), &target));
    if let Err(error) = result {
        let fallback = preserve_or_create_target(&target);
        let warning = match fallback {
            Ok(()) => error,
            Err(fallback_error) => format!("{error}; fallback failed: {fallback_error}"),
        };
        let bounded = warning.chars().take(MAX_WARNING_CHARS).collect::<String>();
        let _ = writeln!(
            std::io::stderr(),
            "system_skills_deployment_warning: {bounded}"
        );
    }
}

fn discover_source() -> Result<Option<PathBuf>, String> {
    if let Some(raw) = std::env::var_os(SOURCE_ENV) {
        let value = raw
            .to_str()
            .ok_or_else(|| format!("{SOURCE_ENV} must be valid UTF-8"))?
            .trim();
        if !value.is_empty() {
            return Ok(Some(PathBuf::from(value)));
        }
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("resolve Runtime executable failed: {error}"))?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| "Runtime executable has no parent directory".to_string())?;
    let direct = executable_dir.join("system-skills");
    if direct.exists() {
        return Ok(Some(direct));
    }
    let packaged = executable_dir
        .parent()
        .map(|parent| parent.join("system-skills"));
    Ok(packaged.filter(|path| path.exists()))
}

fn deploy_from(source: Option<&Path>, target: &Path) -> Result<Deployment, String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("System Skills target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create System Skills parent {} failed: {error}",
            parent.display()
        )
    })?;
    let staging = parent.join(STAGING_NAME);
    let backup = parent.join(BACKUP_NAME);
    recover_interrupted_replace(target, &staging, &backup)?;

    let source_digest = match source {
        Some(source) => inspect_bundle(source)?,
        None => empty_digest(),
    };
    if installed_digest(target).ok().flatten().as_deref() == Some(source_digest.as_str()) {
        return Ok(Deployment {
            digest: source_digest,
            changed: false,
        });
    }

    remove_entry_if_exists(&staging)?;
    fs::create_dir(&staging).map_err(|error| {
        format!(
            "create System Skills staging directory {} failed: {error}",
            staging.display()
        )
    })?;
    let staged = (|| {
        if let Some(source) = source {
            copy_bundle(source, &staging)?;
        }
        let staged_digest = inspect_bundle(&staging)?;
        if staged_digest != source_digest {
            return Err("staged System Skills digest mismatch".to_string());
        }
        crate::atomic_file::write_file_atomically(
            digest_marker_path(target)?.as_path(),
            format!("{source_digest}\n").as_bytes(),
            "System Skills digest marker",
        )?;
        replace_directory(target, &staging, &backup)
    })();
    if staged.is_err() {
        let _ = remove_entry_if_exists(&staging);
    }
    staged?;
    Ok(Deployment {
        digest: source_digest,
        changed: true,
    })
}

fn inspect_bundle(root: &Path) -> Result<String, String> {
    let root_metadata = fs::symlink_metadata(root).map_err(|error| {
        format!(
            "inspect System Skills bundle {} failed: {error}",
            root.display()
        )
    })?;
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Err(format!(
            "System Skills bundle must be a real directory: {}",
            root.display()
        ));
    }

    let mut files = Vec::new();
    for entry in sorted_entries(root)? {
        let name = entry_name(&entry.path())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            format!(
                "inspect System Skill {} failed: {error}",
                entry.path().display()
            )
        })?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            return Err(format!(
                "System Skills bundle root may only contain Skill directories: {name}"
            ));
        }
        let manifest = entry.path().join("SKILL.md");
        let manifest_metadata = fs::symlink_metadata(&manifest)
            .map_err(|_| format!("System Skill is missing SKILL.md: {name}"))?;
        if !manifest_metadata.is_file() || is_link_or_reparse(&manifest_metadata) {
            return Err(format!(
                "System Skill SKILL.md must be a regular file: {name}"
            ));
        }
        collect_files(root, entry.path().as_path(), &mut files)?;
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    digest_files(files)
}

fn installed_digest(target: &Path) -> Result<Option<String>, String> {
    if !target.exists() {
        return Ok(None);
    }
    let marker_path = digest_marker_path(target)?;
    let marker = match fs::read_to_string(&marker_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "read System Skills digest marker {} failed: {error}",
                marker_path.display()
            ));
        }
    };
    let marker = marker.trim();
    if !valid_digest(marker) {
        return Ok(None);
    }
    let actual = inspect_bundle(target)?;
    if actual == marker {
        Ok(Some(actual))
    } else {
        Ok(None)
    }
}

fn copy_bundle(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in sorted_entries(source)? {
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            format!(
                "inspect System Skill source {} failed: {error}",
                entry.path().display()
            )
        })?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            return Err(format!(
                "System Skills bundle root may only contain Skill directories: {}",
                entry_name(&entry.path())?
            ));
        }
        copy_tree(
            entry.path().as_path(),
            destination.join(entry.file_name()).as_path(),
        )?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        format!(
            "inspect System Skill entry {} failed: {error}",
            source.display()
        )
    })?;
    if is_link_or_reparse(&metadata) {
        return Err(format!(
            "System Skill bundle must not contain links or reparse points: {}",
            source.display()
        ));
    }
    if metadata.is_dir() {
        fs::create_dir(destination).map_err(|error| {
            format!(
                "create staged System Skill directory {} failed: {error}",
                destination.display()
            )
        })?;
        for entry in sorted_entries(source)? {
            copy_tree(
                entry.path().as_path(),
                destination.join(entry.file_name()).as_path(),
            )?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!(
            "System Skill bundle contains an unsupported entry: {}",
            source.display()
        ));
    }
    fs::copy(source, destination).map_err(|error| {
        format!(
            "copy System Skill file {} failed: {error}",
            source.display()
        )
    })?;
    Ok(())
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(current).map_err(|error| {
        format!(
            "inspect System Skill entry {} failed: {error}",
            current.display()
        )
    })?;
    if is_link_or_reparse(&metadata) {
        return Err(format!(
            "System Skill bundle must not contain links or reparse points: {}",
            current.display()
        ));
    }
    if metadata.is_dir() {
        for entry in sorted_entries(current)? {
            collect_files(root, entry.path().as_path(), files)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!(
            "System Skill bundle contains an unsupported entry: {}",
            current.display()
        ));
    }
    let relative = current
        .strip_prefix(root)
        .map_err(|error| format!("resolve System Skill relative path failed: {error}"))?;
    let relative = relative
        .to_str()
        .ok_or_else(|| "System Skill paths must be valid UTF-8".to_string())?
        .replace('\\', "/");
    files.push((relative, current.to_path_buf()));
    Ok(())
}

fn digest_marker_path(target: &Path) -> Result<PathBuf, String> {
    target
        .parent()
        .map(|parent| parent.join(DIGEST_MARKER))
        .ok_or_else(|| format!("System Skills target has no parent: {}", target.display()))
}

fn digest_files(files: Vec<(String, PathBuf)>) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    for (relative, path) in files {
        digest.update(relative.as_bytes());
        digest.update(b"\0");
        let file = File::open(&path).map_err(|error| {
            format!("read System Skill file {} failed: {error}", path.display())
        })?;
        let mut reader = BufReader::new(file);
        loop {
            let count = reader.read(&mut buffer).map_err(|error| {
                format!("read System Skill file {} failed: {error}", path.display())
            })?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        digest.update(b"\0");
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn empty_digest() -> String {
    format!("sha256:{:x}", Sha256::digest([]))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn replace_directory(target: &Path, staging: &Path, backup: &Path) -> Result<(), String> {
    remove_entry_if_exists(backup)?;
    if target.exists() {
        fs::rename(target, backup).map_err(|error| {
            format!(
                "move previous System Skills to backup {} failed: {error}",
                backup.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(staging, target) {
        if !target.exists() && backup.exists() {
            let _ = fs::rename(backup, target);
        }
        return Err(format!(
            "activate staged System Skills {} failed: {error}",
            staging.display()
        ));
    }
    remove_entry_if_exists(backup)
}

fn recover_interrupted_replace(target: &Path, staging: &Path, backup: &Path) -> Result<(), String> {
    remove_entry_if_exists(staging)?;
    if backup.exists() {
        if target.exists() {
            remove_entry_if_exists(backup)?;
        } else {
            fs::rename(backup, target).map_err(|error| {
                format!(
                    "restore System Skills backup {} failed: {error}",
                    backup.display()
                )
            })?;
        }
    }
    Ok(())
}

fn preserve_or_create_target(target: &Path) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("System Skills target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create System Skills parent failed: {error}"))?;
    let backup = parent.join(BACKUP_NAME);
    if !target.exists() && backup.exists() {
        fs::rename(&backup, target)
            .map_err(|error| format!("restore System Skills backup failed: {error}"))?;
    }
    if !target.exists() {
        fs::create_dir(target)
            .map_err(|error| format!("create empty System Skills directory failed: {error}"))?;
    }
    Ok(())
}

fn remove_entry_if_exists(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect {} failed: {error}", path.display())),
    };
    let result = if metadata.is_dir() && !is_link_or_reparse(&metadata) {
        fs::remove_dir_all(path)
    } else if metadata.is_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|error| format!("remove {} failed: {error}", path.display()))
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("read {} failed: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read entry under {} failed: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn entry_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("System Skill path must be valid UTF-8: {}", path.display()))
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::runtime::contracts::current_timestamp_ms;

    fn root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris-system-skills-deploy-{label}-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(&root).expect("test root");
        root
    }

    fn write_skill(source: &Path, name: &str, body: &str) {
        let skill = source.join(name);
        fs::create_dir_all(&skill).expect("skill directory");
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test.\n---\n{body}\n"),
        )
        .expect("skill manifest");
    }

    #[test]
    fn deploys_and_updates_valid_bundle_atomically() {
        let root = root("update");
        let source = root.join("source");
        let target = root.join("profile/skills/system");
        fs::create_dir_all(&source).expect("source");
        write_skill(&source, "recovery", "version one");
        let first = deploy_from(Some(&source), &target).expect("first deployment");
        assert!(first.changed);
        assert!(target.join("recovery/SKILL.md").is_file());
        assert_eq!(
            fs::read_to_string(digest_marker_path(&target).expect("marker path"))
                .expect("marker")
                .trim(),
            first.digest
        );

        write_skill(&source, "recovery", "version two");
        let second = deploy_from(Some(&source), &target).expect("updated deployment");
        assert!(second.changed);
        assert_ne!(second.digest, first.digest);
        assert!(fs::read_to_string(target.join("recovery/SKILL.md"))
            .expect("updated manifest")
            .contains("version two"));
        assert!(
            !deploy_from(Some(&source), &target)
                .expect("unchanged deployment")
                .changed
        );
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn invalid_update_preserves_last_known_good() {
        let root = root("preserve");
        let source = root.join("source");
        let target = root.join("profile/skills/system");
        fs::create_dir_all(&source).expect("source");
        write_skill(&source, "recovery", "known good");
        let deployed = deploy_from(Some(&source), &target).expect("deployment");
        fs::remove_file(source.join("recovery/SKILL.md")).expect("break source");

        assert!(deploy_from(Some(&source), &target).is_err());
        assert!(fs::read_to_string(target.join("recovery/SKILL.md"))
            .expect("last known good")
            .contains("known good"));
        assert_eq!(
            fs::read_to_string(digest_marker_path(&target).expect("marker path"))
                .expect("marker")
                .trim(),
            deployed.digest
        );
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn absent_bundle_deploys_an_empty_system_directory() {
        let root = root("empty");
        let target = root.join("profile/skills/system");
        fs::create_dir_all(target.join("stale")).expect("stale skill");
        fs::write(target.join("stale/SKILL.md"), "stale").expect("stale manifest");

        let deployed = deploy_from(None, &target).expect("empty deployment");
        assert!(deployed.changed);
        assert_eq!(deployed.digest, empty_digest());
        assert_eq!(
            fs::read_dir(&target)
                .expect("empty target")
                .filter_map(Result::ok)
                .count(),
            0
        );
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn packaged_empty_bundle_is_valid() {
        let root = root("packaged-empty");
        let source = root.join("source");
        let target = root.join("profile/skills/system");
        fs::create_dir_all(&source).expect("empty source");

        let deployed = deploy_from(Some(&source), &target).expect("empty deployment");
        assert!(deployed.changed);
        assert_eq!(deployed.digest, empty_digest());
        assert_eq!(fs::read_dir(&target).expect("target").count(), 0);
        fs::remove_dir_all(root).expect("remove root");
    }
}
