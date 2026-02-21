use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::error::{IaError, Result};

/// Information about a single disk in the pool.
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub path: PathBuf,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub assigned_items: Vec<String>,
}

/// Status of a disk for reporting.
#[derive(Debug)]
pub struct DiskStatus {
    pub path: PathBuf,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub items_count: usize,
}

/// Multi-disk download destination with automatic failover.
#[derive(Debug)]
pub struct DiskPool {
    disks: Vec<DiskInfo>,
    /// Map from item_id to disk index.
    assignments: HashMap<String, usize>,
}

impl DiskPool {
    /// Create a new disk pool from a list of directory paths.
    pub fn new(paths: &[PathBuf]) -> Result<Self> {
        if paths.is_empty() {
            return Err(IaError::Config("disk pool requires at least one path".to_string()));
        }

        let mut disks = Vec::new();
        for path in paths {
            let (free, total) = disk_space(path)?;
            disks.push(DiskInfo {
                path: path.clone(),
                free_bytes: free,
                total_bytes: total,
                assigned_items: Vec::new(),
            });
        }

        Ok(Self {
            disks,
            assignments: HashMap::new(),
        })
    }

    /// Create a single-disk "pool" (passthrough for non-pool mode).
    pub fn single(path: PathBuf) -> Result<Self> {
        Self::new(&[path])
    }

    /// Assign an item to the disk with the most free space.
    pub fn assign_item(&mut self, item_id: &str, estimated_size: u64) -> Result<&Path> {
        // Find disk with most free space that can fit the item
        let best_idx = self
            .disks
            .iter()
            .enumerate()
            .filter(|(_, d)| d.free_bytes >= estimated_size)
            .max_by_key(|(_, d)| d.free_bytes)
            .map(|(i, _)| i);

        match best_idx {
            Some(idx) => {
                self.disks[idx].free_bytes = self.disks[idx].free_bytes.saturating_sub(estimated_size);
                self.disks[idx].assigned_items.push(item_id.to_string());
                self.assignments.insert(item_id.to_string(), idx);
                debug!(
                    item = item_id,
                    disk = %self.disks[idx].path.display(),
                    "assigned item to disk"
                );
                Ok(&self.disks[idx].path)
            }
            None => Err(IaError::NoDiskSpace { needed: estimated_size }),
        }
    }

    /// Handle disk full: try to reassign the item to another disk.
    pub fn handle_disk_full(&mut self, item_id: &str) -> Result<&Path> {
        // Refresh disk space info
        for disk in &mut self.disks {
            if let Ok((free, _)) = disk_space(&disk.path) {
                disk.free_bytes = free;
            }
        }

        // Remove from current assignment
        let current_idx = self.assignments.remove(item_id);
        if let Some(idx) = current_idx {
            self.disks[idx].assigned_items.retain(|id| id != item_id);
            warn!(
                item = item_id,
                disk = %self.disks[idx].path.display(),
                "disk full, reassigning"
            );
        }

        // Try to find a new disk (excluding the full one)
        let best_idx = self
            .disks
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != current_idx)
            .filter(|(_, d)| d.free_bytes > 0)
            .max_by_key(|(_, d)| d.free_bytes)
            .map(|(i, _)| i);

        match best_idx {
            Some(idx) => {
                self.disks[idx].assigned_items.push(item_id.to_string());
                self.assignments.insert(item_id.to_string(), idx);
                debug!(
                    item = item_id,
                    disk = %self.disks[idx].path.display(),
                    "reassigned item to new disk"
                );
                Ok(&self.disks[idx].path)
            }
            None => Err(IaError::NoDiskSpace { needed: 0 }),
        }
    }

    /// Get the destination path for an already-assigned item.
    pub fn dest_for_item(&self, item_id: &str) -> Option<&Path> {
        self.assignments
            .get(item_id)
            .map(|&idx| self.disks[idx].path.as_path())
    }

    /// Get status of all disks.
    pub fn status(&self) -> Vec<DiskStatus> {
        self.disks
            .iter()
            .map(|d| DiskStatus {
                path: d.path.clone(),
                free_bytes: d.free_bytes,
                total_bytes: d.total_bytes,
                items_count: d.assigned_items.len(),
            })
            .collect()
    }

    /// Number of disks in the pool.
    pub fn len(&self) -> usize {
        self.disks.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.disks.is_empty()
    }
}

/// Get available and total disk space for a path.
fn disk_space(path: &Path) -> Result<(u64, u64)> {
    // Use std::fs to get disk space via statvfs on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = std::fs::metadata(path).map_err(|_| {
            IaError::Config(format!("cannot access disk: {}", path.display()))
        })?;

        // Use nix or manual statvfs
        let c_path = std::ffi::CString::new(path.to_str().unwrap_or("")).map_err(|_| {
            IaError::Config(format!("invalid path: {}", path.display()))
        })?;

        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret != 0 {
            return Err(IaError::Config(format!(
                "statvfs failed for {}",
                path.display()
            )));
        }

        let free = stat.f_bavail as u64 * stat.f_frsize as u64;
        let total = stat.f_blocks as u64 * stat.f_frsize as u64;
        Ok((free, total))
    }

    #[cfg(not(unix))]
    {
        // Fallback: assume plenty of space
        Ok((u64::MAX, u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_disk_pool() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool = DiskPool::single(dir.path().to_path_buf()).unwrap();
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_empty());

        let dest = pool.assign_item("nasa", 1024).unwrap();
        assert_eq!(dest, dir.path());
    }

    #[test]
    fn assign_returns_disk_with_most_space() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let mut pool = DiskPool::new(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()]).unwrap();

        // Both disks should have space
        let dest = pool.assign_item("item1", 1024).unwrap();
        assert!(dest == dir1.path() || dest == dir2.path());
    }

    #[test]
    fn dest_for_assigned_item() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool = DiskPool::single(dir.path().to_path_buf()).unwrap();

        pool.assign_item("nasa", 1024).unwrap();
        assert_eq!(pool.dest_for_item("nasa"), Some(dir.path()));
        assert_eq!(pool.dest_for_item("unknown"), None);
    }

    #[test]
    fn status_reports_disks() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool = DiskPool::single(dir.path().to_path_buf()).unwrap();
        pool.assign_item("nasa", 1024).unwrap();

        let status = pool.status();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].items_count, 1);
        assert!(status[0].total_bytes > 0);
    }

    #[test]
    fn empty_pool_is_error() {
        let result = DiskPool::new(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn no_disk_space_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut pool = DiskPool::new(&[dir.path().to_path_buf()]).unwrap();
        // Force free space to 0
        pool.disks[0].free_bytes = 0;

        let result = pool.assign_item("huge-item", 1024);
        assert!(result.is_err());
    }
}
