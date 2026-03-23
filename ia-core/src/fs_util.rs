//! Local filesystem utilities for expanding file/directory arguments.

use std::path::{Path, PathBuf};

use crate::Result;

/// Expand a list of file and directory paths into individual file paths.
///
/// Directories are recursively walked. Dotfiles/dotdirs and symlinks are
/// skipped. Results are sorted for deterministic ordering.
pub fn expand_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for path in paths {
        if path.is_dir() {
            walk_dir(path, &mut result)?;
        } else {
            result.push(path.clone());
        }
    }
    result.sort();
    Ok(result)
}

/// Recursively walk a directory, collecting regular files.
///
/// Skips symlinks and entries whose filename starts with `.`.
pub fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        // Skip dotfiles and dotdirs
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }

        // Skip symlinks
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }

        if meta.is_dir() {
            walk_dir(&path, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn expand_single_file() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("test.txt");
        fs::write(&f, "content").unwrap();

        let result = expand_files(&[f.clone()]).unwrap();
        assert_eq!(result, vec![f]);
    }

    #[test]
    fn expand_directory_skips_dotfiles() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("visible.txt"), "yes").unwrap();
        fs::write(dir.path().join(".hidden"), "no").unwrap();

        let result = expand_files(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].file_name().unwrap().to_str().unwrap() == "visible.txt");
    }

    #[test]
    fn expand_directory_recursive() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(sub.join("b.txt"), "b").unwrap();

        let result = expand_files(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn expand_empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let result = expand_files(&[dir.path().to_path_buf()]).unwrap();
        assert!(result.is_empty());
    }
}
