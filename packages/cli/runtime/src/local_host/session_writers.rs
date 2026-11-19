use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) struct SessionWriters {
    directory: PathBuf,
    claims: Mutex<HashMap<String, Arc<File>>>,
}

impl SessionWriters {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            directory: data_root.join("runtime/session-writers"),
            claims: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn try_claim(&self, session_id: &str) -> Result<bool, String> {
        let root =
            session_root(session_id)?.ok_or_else(|| format!("Session not found: {session_id}"))?;
        let mut claims = self
            .claims
            .lock()
            .map_err(|_| "Session writer table lock poisoned".to_string())?;
        if claims.contains_key(&root) {
            return Ok(true);
        }
        let key = Sha256::digest(root.as_bytes());
        let file = open_lock(&self.directory.join(format!("{key:x}.lock")))?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                return Ok(false)
            }
            Err(error) => return Err(format!("Acquire Session writer failed: {error}")),
        }

        // Recover the prior owner only while the operating-system lease is ours.
        let family = session_family(&root)?;
        crate::agent_runtime::recover_live_text_for_sessions(&family)?;
        claims.insert(root, Arc::new(file));
        Ok(true)
    }

    pub(crate) fn owned_roots(&self) -> Result<Vec<String>, String> {
        let claims = self
            .claims
            .lock()
            .map_err(|_| "Session writer table lock poisoned".to_string())?;
        Ok(claims.keys().cloned().collect())
    }

    pub(crate) fn pin(&self, session_id: &str) -> Result<Option<Arc<File>>, String> {
        let claims = self
            .claims
            .lock()
            .map_err(|_| "Session writer table lock poisoned".to_string())?;
        if let Some(file) = claims.get(session_id) {
            return Ok(Some(Arc::clone(file)));
        }
        let Some(root) = session_root(session_id)? else {
            return Ok(None);
        };
        Ok(claims.get(&root).cloned())
    }

    pub(crate) fn release(&self, root: &str) -> Result<(), String> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|_| "Session writer table lock poisoned".to_string())?;
        if claims
            .get(root)
            .is_some_and(|file| Arc::strong_count(file) == 1)
        {
            claims.remove(root);
        }
        Ok(())
    }
}

fn session_root(session_id: &str) -> Result<Option<String>, String> {
    let mut id = session_id.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(id.clone()) {
            return Err(format!("Session parent cycle at {id}"));
        }
        let Some(item) = crate::sessions::listed_session(&id)? else {
            return Ok(None);
        };
        match item.parent_session_id {
            Some(parent) => id = parent,
            None => return Ok(Some(id)),
        }
    }
}

fn session_family(root: &str) -> Result<HashSet<String>, String> {
    let items =
        crate::session_files::SessionFiles::new(crate::user_data_layout::sessions_dir_path())
            .list()?;
    let mut children = HashMap::<String, Vec<String>>::new();
    for item in items {
        if let Some(parent) = item.parent_session_id {
            children.entry(parent).or_default().push(item.id);
        }
    }
    let mut sessions = HashSet::new();
    let mut pending = vec![root.to_string()];
    while let Some(id) = pending.pop() {
        if let Some(children) = children.remove(&id) {
            pending.extend(children);
        }
        sessions.insert(id);
    }
    Ok(sessions)
}

pub(super) fn open_lock(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Create lock directory failed: {error}"))?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("Open lock {} failed: {error}", path.display()))
}
