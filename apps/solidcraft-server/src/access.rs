//! Who may see and change which designs. A design or a folder can be shared with certain
//! people: an entry names its owner, who may view it and who may edit it (`None`: everyone).
//! A design without an entry of its own follows the nearest folder with one, else the
//! default: everyone may do everything (`SOLIDCRAFT_DEFAULT_ACCESS=everyone`, the usual for a
//! household), or only the one who created it (`owner`: every new design gets an entry for
//! its creator). Owners and admins always have full access and are the ones who change the
//! sharing. Kept in `<history>/access.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Sharing of one design (its path) or folder (its path with a trailing slash; "/" is the root).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub owner: String,
    /// Who may view (besides the owner and admins); `None`: everyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<Vec<String>>,
    /// Who may change (besides the owner and admins); `None`: everyone who may view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    None,
    View,
    Edit,
}

pub struct Acl {
    file: PathBuf,
    entries: Mutex<BTreeMap<String, Entry>>,
    /// Without any entry: everyone may do everything (else only the creator).
    pub default_open: bool,
}

/// The folder a path is in, as an entry key ("a/b/" for "a/b/c", "/" at the top), or `None`
/// for the top folder itself.
fn parent(key: &str) -> Option<String> {
    let trimmed = key.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(match trimmed.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => "/".to_string(),
    })
}

/// A design path, or a folder path with a trailing slash, as an entry key.
pub fn key(path: &str, folder: bool) -> String {
    match (path, folder) {
        ("", _) => "/".to_string(),
        (p, true) => format!("{p}/"),
        (p, false) => p.to_string(),
    }
}

impl Acl {
    pub fn open(file: &Path, default_open: bool) -> Self {
        let entries = std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { file: file.to_path_buf(), entries: Mutex::new(entries), default_open }
    }

    fn save(&self, entries: &BTreeMap<String, Entry>) {
        match serde_json::to_vec_pretty(entries) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&self.file, bytes) {
                    log::warn!("{}: {e}", self.file.display());
                }
            }
            Err(e) => log::warn!("access: {e}"),
        }
    }

    /// The entry of a key itself.
    pub fn get(&self, key: &str) -> Option<Entry> {
        self.entries.lock().ok()?.get(key).cloned()
    }

    /// The entry that applies to a key: its own, else the nearest folder's: (that key, entry).
    pub fn effective(&self, key: &str) -> Option<(String, Entry)> {
        let entries = self.entries.lock().ok()?;
        let mut at = Some(key.to_string());
        while let Some(k) = at {
            if let Some(e) = entries.get(&k) {
                return Some((k, e.clone()));
            }
            at = parent(&k);
        }
        None
    }

    pub fn set(&self, key: &str, entry: Entry) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(key.to_string(), entry);
            self.save(&entries);
        }
    }

    pub fn remove(&self, key: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(key);
            self.save(&entries);
        }
    }

    /// What `user` may do with a key.
    pub fn level(&self, user: &str, admin: bool, key: &str) -> Level {
        if admin {
            return Level::Edit;
        }
        let Some((_, e)) = self.effective(key) else {
            return if self.default_open { Level::Edit } else { Level::View.max(Level::None) };
        };
        if e.owner == user {
            return Level::Edit;
        }
        let listed = |l: &Option<Vec<String>>| l.as_ref().is_none_or(|l| l.iter().any(|u| u == user));
        if !listed(&e.view) && !listed(&e.edit) {
            return Level::None;
        }
        if listed(&e.edit) { Level::Edit } else { Level::View }
    }

    /// What others shared with `user` by name: (key, owner, may edit), folders and designs.
    pub fn shared_with(&self, user: &str) -> Vec<(String, String, bool)> {
        let Ok(entries) = self.entries.lock() else { return Vec::new() };
        let listed = |l: &Option<Vec<String>>| l.as_ref().is_some_and(|l| l.iter().any(|u| u == user));
        entries
            .iter()
            .filter(|(_, e)| e.owner != user && (listed(&e.view) || listed(&e.edit)))
            .map(|(k, e)| (k.clone(), e.owner.clone(), listed(&e.edit)))
            .collect()
    }

    /// May `user` change the sharing of a key? Admins, and the owner of its own entry.
    pub fn manages(&self, user: &str, admin: bool, key: &str) -> bool {
        admin || self.get(key).is_some_and(|e| e.owner == user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_cover_their_designs_and_owners_rule() {
        let file = std::env::temp_dir().join(format!("solidcraft-access-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let acl = Acl::open(&file, true);
        assert_eq!(acl.level("ben", false, "a/x.solidcraft"), Level::Edit, "open by default");
        acl.set("a/", Entry { owner: "ben".into(), view: Some(vec!["felix".into()]), edit: Some(vec![]) });
        assert_eq!(acl.level("ben", false, "a/x.solidcraft"), Level::Edit, "the owner");
        assert_eq!(acl.level("felix", false, "a/x.solidcraft"), Level::View, "listed to view");
        assert_eq!(acl.level("eve", false, "a/x.solidcraft"), Level::None);
        assert_eq!(acl.level("eve", true, "a/x.solidcraft"), Level::Edit, "an admin");
        acl.set("a/x.solidcraft", Entry { owner: "ben".into(), view: None, edit: Some(vec!["eve".into()]) });
        assert_eq!(acl.level("eve", false, "a/x.solidcraft"), Level::Edit, "the design's own entry wins");
        assert_eq!(acl.level("eve", false, "a/y.solidcraft"), Level::None, "the folder's still covers the rest");
        assert_eq!(acl.effective("a/b/z.solidcraft").map(|(k, _)| k), Some("a/".to_string()));
        assert!(acl.manages("ben", false, "a/") && !acl.manages("felix", false, "a/") && acl.manages("felix", true, "a/"));
        assert_eq!(acl.shared_with("felix"), vec![("a/".to_string(), "ben".to_string(), false)]);
        assert_eq!(acl.shared_with("eve"), vec![("a/x.solidcraft".to_string(), "ben".to_string(), true)]);
        assert!(acl.shared_with("ben").is_empty(), "one's own entries are not shares");
        assert_eq!(key("", true), "/");
        assert_eq!(parent("a/b/"), Some("a/".to_string()));
        assert_eq!(parent("a/"), Some("/".to_string()));
        assert_eq!(parent("/"), None);
        // It is read back.
        let again = Acl::open(&file, true);
        assert_eq!(again.get("a/").map(|e| e.owner), Some("ben".to_string()));
        let _ = std::fs::remove_file(&file);
    }
}
