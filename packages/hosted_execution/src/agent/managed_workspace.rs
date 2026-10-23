//! Initialize a separately owned workspace without removing template contents.
#[cfg(any(test, target_os = "linux"))]
use serde::{Deserialize, Serialize};

#[cfg(any(test, target_os = "linux"))]
const MARKER_SCHEMA: &str = "sandbox.managed_workspace.v1";
#[cfg(any(test, target_os = "linux"))]
const MARKER_MAX_BYTES: usize = 16 * 1024;

#[cfg(target_os = "linux")]
mod linux;

pub(super) fn prepare() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::prepare()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("managed workspace initialization requires Linux".into())
    }
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Scope {
    environment_id: String,
    sandbox_id: String,
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum InitializationState {
    Preparing,
    Prepared,
    Ready,
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Marker {
    schema: String,
    scope: Scope,
    state: InitializationState,
    #[serde(deserialize_with = "deserialize_backing")]
    backing: Option<DirectoryIdentity>,
}

#[cfg(any(test, target_os = "linux"))]
fn deserialize_backing<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DirectoryIdentity>, D::Error> {
    Option::<DirectoryIdentity>::deserialize(deserializer)
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MountState {
    Unbound,
    Backing,
    Foreign,
}

// The caller owns the exclusive initialization lock for the entire transaction.
#[cfg(any(test, target_os = "linux"))]
trait WorkspaceBackend {
    fn marker(&mut self) -> Result<Option<Marker>, String>;
    fn backing(&mut self) -> Result<Option<DirectoryIdentity>, String>;
    fn mount_state(&mut self) -> Result<MountState, String>;
    fn write_marker(&mut self, marker: &Marker) -> Result<(), String>;
    fn create_backing(&mut self) -> Result<DirectoryIdentity, String>;
    fn backing_is_empty(&mut self) -> Result<bool, String>;
    fn bind_backing(&mut self) -> Result<(), String>;
}

#[cfg(any(test, target_os = "linux"))]
impl Scope {
    fn validate(&self) -> Result<(), String> {
        for identity in [&self.environment_id, &self.sandbox_id] {
            if identity.is_empty()
                || identity.len() > 256
                || identity.trim() != identity
                || identity.chars().any(char::is_control)
            {
                return Err("managed_workspace_scope_invalid".into());
            }
        }
        Ok(())
    }
}

#[cfg(any(test, target_os = "linux"))]
fn parse_marker(bytes: &[u8]) -> Result<Marker, String> {
    if bytes.is_empty() || bytes.len() > MARKER_MAX_BYTES {
        return Err("managed_workspace_marker_invalid".into());
    }
    serde_json::from_slice(bytes).map_err(|_| "managed_workspace_marker_invalid".into())
}

#[cfg(any(test, target_os = "linux"))]
fn initialize(scope: &Scope, backend: &mut impl WorkspaceBackend) -> Result<(), String> {
    scope.validate()?;
    let mut marker = match backend.marker()? {
        Some(marker) => marker,
        None => {
            if backend.backing()?.is_some() || backend.mount_state()? != MountState::Unbound {
                return Err("managed_workspace_unowned_state".into());
            }
            let marker = Marker {
                schema: MARKER_SCHEMA.into(),
                scope: scope.clone(),
                state: InitializationState::Preparing,
                backing: None,
            };
            backend.write_marker(&marker)?;
            marker
        }
    };
    if marker.schema != MARKER_SCHEMA || marker.scope != *scope {
        return Err("managed_workspace_marker_scope_mismatch".into());
    }
    if marker.state == InitializationState::Preparing {
        if marker.backing.is_some() || backend.mount_state()? != MountState::Unbound {
            return Err("managed_workspace_preparation_invalid".into());
        }
        let backing = match backend.backing()? {
            Some(identity) => {
                if !backend.backing_is_empty()? {
                    return Err("managed_workspace_preparation_not_empty".into());
                }
                identity
            }
            None => backend.create_backing()?,
        };
        if !backend.backing_is_empty()? {
            return Err("managed_workspace_preparation_not_empty".into());
        }
        marker.backing = Some(backing);
        marker.state = InitializationState::Prepared;
        backend.write_marker(&marker)?;
    }
    let identity = marker.backing.ok_or("managed_workspace_backing_missing")?;
    if backend.backing()? != Some(identity) {
        return Err("managed_workspace_backing_identity_mismatch".into());
    }
    match backend.mount_state()? {
        MountState::Unbound => {
            // Even a ready marker permits only rebinding the exact recorded directory.
            marker.state = InitializationState::Prepared;
            backend.write_marker(&marker)?;
            backend.bind_backing()?;
        }
        MountState::Backing => {}
        MountState::Foreign => return Err("managed_workspace_foreign_mount".into()),
    }
    if backend.mount_state()? != MountState::Backing {
        return Err("managed_workspace_mount_unverified".into());
    }
    if marker.state != InitializationState::Ready {
        marker.state = InitializationState::Ready;
        backend.write_marker(&marker)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    struct FakeMount {
        marker: Option<Marker>,
        backing: Option<DirectoryIdentity>,
        mount: MountState,
        template_files: BTreeMap<String, Vec<u8>>,
        backing_files: BTreeMap<String, Vec<u8>>,
        creates: usize,
        binds: usize,
        lose_mount_reply: bool,
        lose_create_reply: bool,
        unsafe_backing: bool,
    }

    impl Default for FakeMount {
        fn default() -> Self {
            Self {
                marker: None,
                backing: None,
                mount: MountState::Unbound,
                template_files: BTreeMap::from([(
                    "AGENTS.md".into(),
                    b"template instructions".to_vec(),
                )]),
                backing_files: BTreeMap::new(),
                creates: 0,
                binds: 0,
                lose_mount_reply: false,
                lose_create_reply: false,
                unsafe_backing: false,
            }
        }
    }

    fn scope() -> Scope {
        Scope {
            environment_id: "environment-1".into(),
            sandbox_id: "sandbox-1".into(),
        }
    }

    impl WorkspaceBackend for FakeMount {
        fn marker(&mut self) -> Result<Option<Marker>, String> {
            Ok(self.marker.clone())
        }
        fn backing(&mut self) -> Result<Option<DirectoryIdentity>, String> {
            if self.unsafe_backing {
                return Err("managed_workspace_backing_unsafe".into());
            }
            Ok(self.backing)
        }
        fn mount_state(&mut self) -> Result<MountState, String> {
            Ok(self.mount)
        }
        fn write_marker(&mut self, marker: &Marker) -> Result<(), String> {
            self.marker = Some(marker.clone());
            Ok(())
        }
        fn create_backing(&mut self) -> Result<DirectoryIdentity, String> {
            assert!(
                self.marker.is_some(),
                "durable scope intent must precede creating a directory"
            );
            assert!(
                self.backing.is_none(),
                "existing backing must never be replaced"
            );
            self.creates += 1;
            let identity = DirectoryIdentity {
                device: 7,
                inode: 42,
            };
            self.backing = Some(identity);
            if std::mem::take(&mut self.lose_create_reply) {
                return Err("create reply lost".into());
            }
            Ok(identity)
        }
        fn backing_is_empty(&mut self) -> Result<bool, String> {
            Ok(self.backing_files.is_empty())
        }
        fn bind_backing(&mut self) -> Result<(), String> {
            assert_eq!(
                self.marker.as_ref().unwrap().state,
                InitializationState::Prepared
            );
            self.binds += 1;
            self.mount = MountState::Backing;
            if std::mem::take(&mut self.lose_mount_reply) {
                return Err("mount reply lost".into());
            }
            Ok(())
        }
    }

    #[test]
    fn managed_initialization_hides_template_files_without_removing_them() {
        let mut host = FakeMount::default();
        initialize(&scope(), &mut host).unwrap();
        assert_eq!(host.mount, MountState::Backing);
        assert!(host.backing_files.is_empty());
        assert_eq!(host.template_files["AGENTS.md"], b"template instructions");
        assert_eq!(host.marker.unwrap().state, InitializationState::Ready);
    }

    #[test]
    fn repeated_initialization_preserves_restored_and_new_files() {
        let mut host = FakeMount::default();
        initialize(&scope(), &mut host).unwrap();
        host.backing_files
            .insert("result.txt".into(), b"do not clear".to_vec());
        let identity = host.backing;
        initialize(&scope(), &mut host).unwrap();
        assert_eq!(host.backing_files["result.txt"], b"do not clear");
        assert_eq!(host.backing, identity);
        assert_eq!((host.creates, host.binds), (1, 1));
    }

    #[test]
    fn lost_mount_reply_reuses_the_recorded_directory() {
        let mut host = FakeMount {
            lose_mount_reply: true,
            ..FakeMount::default()
        };
        assert!(initialize(&scope(), &mut host).is_err());
        assert_eq!(
            host.marker.as_ref().unwrap().state,
            InitializationState::Prepared
        );
        host.backing_files
            .insert("receipt.txt".into(), b"preserve".to_vec());
        initialize(&scope(), &mut host).unwrap();
        assert_eq!(host.backing_files["receipt.txt"], b"preserve");
        assert_eq!((host.creates, host.binds), (1, 1));
    }

    #[test]
    fn lost_reply_after_backing_permissions_are_sealed_recovers_only_empty_reserved_directory() {
        let mut host = FakeMount {
            lose_create_reply: true,
            ..FakeMount::default()
        };
        assert!(initialize(&scope(), &mut host).is_err());
        assert_eq!(
            host.marker.as_ref().unwrap().state,
            InitializationState::Preparing
        );
        initialize(&scope(), &mut host).unwrap();
        assert_eq!((host.creates, host.binds), (1, 1));

        let mut unsafe_host = FakeMount {
            lose_create_reply: true,
            ..FakeMount::default()
        };
        assert!(initialize(&scope(), &mut unsafe_host).is_err());
        unsafe_host
            .backing_files
            .insert("unknown.txt".into(), b"retain responsibility".to_vec());
        assert!(initialize(&scope(), &mut unsafe_host).is_err());
        assert_eq!(
            unsafe_host.backing_files["unknown.txt"],
            b"retain responsibility"
        );
        assert_eq!((unsafe_host.creates, unsafe_host.binds), (1, 0));
    }

    #[test]
    fn partial_backing_preparation_remains_owned_and_fails_closed_without_reset() {
        let mut host = FakeMount {
            lose_create_reply: true,
            ..FakeMount::default()
        };
        assert!(initialize(&scope(), &mut host).is_err());
        // A crash between mkdir and sealed uid/gid/mode is not a completed creation reply loss.
        host.unsafe_backing = true;
        host.backing_files
            .insert("unknown.txt".into(), b"preserve evidence".to_vec());
        assert!(initialize(&scope(), &mut host).is_err());
        assert_eq!(
            host.marker.as_ref().unwrap().state,
            InitializationState::Preparing
        );
        assert_eq!(host.backing_files["unknown.txt"], b"preserve evidence");
        assert_eq!((host.creates, host.binds), (1, 0));
    }

    #[test]
    fn a_missing_mount_rebinds_the_same_ready_backing_without_clearing_it() {
        let mut host = FakeMount::default();
        initialize(&scope(), &mut host).unwrap();
        host.mount = MountState::Unbound;
        host.backing_files
            .insert("saved.txt".into(), b"saved".to_vec());
        initialize(&scope(), &mut host).unwrap();
        assert_eq!(host.backing_files["saved.txt"], b"saved");
        assert_eq!((host.creates, host.binds), (1, 2));
    }

    #[test]
    fn inherited_scope_foreign_mount_and_unowned_backing_fail_closed() {
        let mut inherited = FakeMount::default();
        initialize(&scope(), &mut inherited).unwrap();
        let mut other = scope();
        other.sandbox_id = "different-sandbox".into();
        assert!(initialize(&other, &mut inherited).is_err());
        assert_eq!((inherited.creates, inherited.binds), (1, 1));

        for mount in [MountState::Foreign, MountState::Backing] {
            let mut host = FakeMount {
                mount,
                ..FakeMount::default()
            };
            assert!(initialize(&scope(), &mut host).is_err());
            assert!(host.marker.is_none());
            assert_eq!((host.creates, host.binds), (0, 0));
        }
        let mut host = FakeMount {
            backing: Some(DirectoryIdentity {
                device: 7,
                inode: 43,
            }),
            ..FakeMount::default()
        };
        assert!(initialize(&scope(), &mut host).is_err());
        assert!(host.marker.is_none());
        assert_eq!((host.creates, host.binds), (0, 0));
    }

    #[test]
    fn marker_and_scope_reject_ambiguous_or_unsupported_state() {
        let marker = Marker {
            schema: MARKER_SCHEMA.into(),
            scope: scope(),
            state: InitializationState::Preparing,
            backing: None,
        };
        let raw = serde_json::to_vec(&marker).unwrap();
        assert_eq!(parse_marker(&raw).unwrap(), marker);
        let mut value = serde_json::to_value(&marker).unwrap();
        value["extra"] = true.into();
        assert!(parse_marker(&serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("extra");
        value.as_object_mut().unwrap().remove("backing");
        assert!(parse_marker(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(parse_marker(&vec![b' '; MARKER_MAX_BYTES + 1]).is_err());
        assert!(parse_marker(br#"{"schema":"one","schema":"two"}"#).is_err());
        for identity in [
            "".to_string(),
            " leading".into(),
            "trailing ".into(),
            "bad\nidentity".into(),
            "x".repeat(257),
        ] {
            let mut scope = scope();
            scope.environment_id = identity.clone();
            assert!(scope.validate().is_err());
            scope = super::tests::scope();
            scope.sandbox_id = identity;
            assert!(scope.validate().is_err());
        }
    }

    #[test]
    fn concurrent_initializers_share_one_backing_under_the_exclusive_lock() {
        let host = Arc::new(Mutex::new(FakeMount::default()));
        let handles = (0..8)
            .map(|_| {
                let host = host.clone();
                std::thread::spawn(move || initialize(&scope(), &mut *host.lock().unwrap()))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let host = host.lock().unwrap();
        assert_eq!((host.creates, host.binds), (1, 1));
        assert_eq!(
            host.marker.as_ref().unwrap().state,
            InitializationState::Ready
        );
    }
}
