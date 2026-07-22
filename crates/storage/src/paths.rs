use directories::ProjectDirs;
use std::path::{Path, PathBuf};

use crate::{Result, StorageError};

#[derive(Debug, Clone)]
pub struct DbPaths {
    pub root: PathBuf,
    pub db_path: PathBuf,
    pub artifacts_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

impl DbPaths {
    pub fn default_app() -> Result<Self> {
        let dirs = ProjectDirs::from("app", "TestHarbor", "TestHarbor").ok_or_else(|| {
            StorageError::Other("cannot resolve Application Support directory".into())
        })?;
        Self::from_root(dirs.data_dir())
    }

    pub fn from_root(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        Ok(Self {
            db_path: root.join("testharbor.db"),
            artifacts_dir: root.join("artifacts"),
            worktrees_dir: root.join("worktrees"),
            root,
        })
    }
}

/// Public helper used by application layer.
pub fn data_root() -> Result<PathBuf> {
    Ok(DbPaths::default_app()?.root)
}
