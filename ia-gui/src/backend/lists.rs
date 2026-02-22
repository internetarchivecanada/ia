use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A named list of item identifiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemList {
    pub name: String,
    pub identifiers: Vec<String>,
}

/// Manages persistent named lists of identifiers.
pub struct ListManager {
    dir: PathBuf,
}

impl ListManager {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        Self { dir }
    }

    /// Default lists directory.
    pub fn default_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ia")
            .join("lists")
    }

    /// List all saved lists (names only).
    pub fn list_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names.sort();
        names
    }

    /// Load a list by name.
    pub fn load(&self, name: &str) -> Option<ItemList> {
        let path = self.list_path(name);
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save a list.
    pub fn save(&self, list: &ItemList) -> std::io::Result<()> {
        let path = self.list_path(&list.name);
        let content = serde_json::to_string_pretty(list)
            .map_err(std::io::Error::other)?;
        std::fs::write(path, content)
    }

    /// Create a new empty list.
    pub fn create(&self, name: &str) -> std::io::Result<ItemList> {
        let list = ItemList {
            name: name.to_string(),
            identifiers: Vec::new(),
        };
        self.save(&list)?;
        Ok(list)
    }

    /// Delete a list by name.
    pub fn delete(&self, name: &str) -> std::io::Result<()> {
        let path = self.list_path(name);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Add identifiers to a list, deduplicating.
    pub fn add_identifiers(&self, name: &str, ids: &[String]) -> std::io::Result<ItemList> {
        let mut list = self.load(name).unwrap_or_else(|| ItemList {
            name: name.to_string(),
            identifiers: Vec::new(),
        });

        let existing: HashSet<String> = list.identifiers.iter().cloned().collect();
        for id in ids {
            if !existing.contains(id) {
                list.identifiers.push(id.clone());
            }
        }

        self.save(&list)?;
        Ok(list)
    }

    /// Import identifiers from a plain text file (one per line).
    #[allow(dead_code)]
    pub fn import_text(&self, name: &str, path: &Path) -> std::io::Result<ItemList> {
        let content = std::fs::read_to_string(path)?;
        let ids: Vec<String> = content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        self.add_identifiers(name, &ids)
    }

    /// Import identifiers from a JSONL file (objects with "identifier" field).
    #[allow(dead_code)]
    pub fn import_jsonl(&self, name: &str, path: &Path) -> std::io::Result<ItemList> {
        let content = std::fs::read_to_string(path)?;
        let ids: Vec<String> = content
            .lines()
            .filter_map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("identifier")?.as_str().map(|s| s.to_string()))
            })
            .collect();
        self.add_identifiers(name, &ids)
    }

    fn list_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> (tempfile::TempDir, ListManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ListManager::new(dir.path().to_path_buf());
        (dir, mgr)
    }

    #[test]
    fn test_create_and_load() {
        let (_dir, mgr) = test_manager();
        let list = mgr.create("my-list").unwrap();
        assert_eq!(list.name, "my-list");
        assert!(list.identifiers.is_empty());

        let loaded = mgr.load("my-list").unwrap();
        assert_eq!(loaded.name, "my-list");
    }

    #[test]
    fn test_list_names() {
        let (_dir, mgr) = test_manager();
        mgr.create("alpha").unwrap();
        mgr.create("beta").unwrap();
        let names = mgr.list_names();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_add_identifiers_dedup() {
        let (_dir, mgr) = test_manager();
        mgr.create("test").unwrap();
        mgr.add_identifiers("test", &["a".into(), "b".into()])
            .unwrap();
        let list = mgr
            .add_identifiers("test", &["b".into(), "c".into()])
            .unwrap();
        assert_eq!(list.identifiers, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_delete() {
        let (_dir, mgr) = test_manager();
        mgr.create("deleteme").unwrap();
        assert!(mgr.load("deleteme").is_some());
        mgr.delete("deleteme").unwrap();
        assert!(mgr.load("deleteme").is_none());
    }

    #[test]
    fn test_import_text() {
        let (dir, mgr) = test_manager();
        let file = dir.path().join("ids.txt");
        std::fs::write(&file, "item1\nitem2\n\nitem3\n").unwrap();
        let list = mgr.import_text("imported", &file).unwrap();
        assert_eq!(list.identifiers, vec!["item1", "item2", "item3"]);
    }

    #[test]
    fn test_import_jsonl() {
        let (dir, mgr) = test_manager();
        let file = dir.path().join("results.jsonl");
        std::fs::write(
            &file,
            "{\"identifier\":\"a\",\"title\":\"A\"}\n{\"identifier\":\"b\",\"title\":\"B\"}\n",
        )
        .unwrap();
        let list = mgr.import_jsonl("imported", &file).unwrap();
        assert_eq!(list.identifiers, vec!["a", "b"]);
    }

    #[test]
    fn test_load_nonexistent() {
        let (_dir, mgr) = test_manager();
        assert!(mgr.load("nonexistent").is_none());
    }
}
