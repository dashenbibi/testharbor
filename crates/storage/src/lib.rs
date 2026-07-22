//! SQLite-backed storage. Events are the authoritative workflow log.

mod paths;
mod schema;

pub use paths::{data_root, DbPaths};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use testharbor_domain::{
    DeliveryStatus, ProjectId, ResultVerdict, RunId, RunPointers, WorkflowState,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: ProjectId,
    pub root_path: String,
    pub name: String,
    pub git_remote: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: RunId,
    pub project_id: ProjectId,
    pub objective: String,
    pub workflow_state: WorkflowState,
    pub delivery_status: DeliveryStatus,
    pub result_verdict: Option<ResultVerdict>,
    pub base_commit: String,
    pub worktree_path: Option<String>,
    pub test_intent_hash: Option<String>,
    pub config_snapshot_hash: Option<String>,
    pub pointers_json: String,
    pub snapshot_version: Option<u32>,
    pub legacy_read_only: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: i64,
    pub run_id: Option<RunId>,
    pub project_id: Option<ProjectId>,
    pub event_type: String,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStateRecord {
    pub run_id: RunId,
    pub state_key: String,
    pub payload_json: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionHistoryRecord {
    pub id: String,
    pub purpose: String,
    pub status: String,
    pub scope_hash: String,
    pub payload_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveProcessTaskRecord {
    pub id: String,
    pub run_id: RunId,
    pub slot: String,
    pub status: String,
    pub payload_json: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityLeaseRecord {
    pub run_id: RunId,
    pub activity_kind: String,
    pub owner_id: String,
    pub owner_pid: u32,
    pub owner_start_token: String,
    pub owner_executable_identity: String,
    pub acquired_at: DateTime<Utc>,
}

pub struct Store {
    conn: Connection,
    paths: DbPaths,
}

impl Store {
    pub fn transaction<T>(&self, operation: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match operation(self) {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Open (or create) the app database under Application Support.
    pub fn open_default() -> Result<Self> {
        let paths = DbPaths::default_app()?;
        Self::open_paths(paths)
    }

    /// Open a store at an explicit root (tests).
    pub fn open_at(root: impl AsRef<Path>) -> Result<Self> {
        let paths = DbPaths::from_root(root.as_ref())?;
        Self::open_paths(paths)
    }

    fn open_paths(paths: DbPaths) -> Result<Self> {
        std::fs::create_dir_all(&paths.root)?;
        std::fs::create_dir_all(&paths.artifacts_dir)?;
        std::fs::create_dir_all(&paths.worktrees_dir)?;
        let conn = Connection::open(&paths.db_path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            ",
        )?;
        schema::migrate(&conn)?;
        Ok(Self { conn, paths })
    }

    pub fn paths(&self) -> &DbPaths {
        &self.paths
    }

    pub fn upsert_project(&self, project: &ProjectRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects (id, root_path, name, git_remote, created_at, last_opened_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               root_path = excluded.root_path,
               name = excluded.name,
               git_remote = excluded.git_remote,
               last_opened_at = excluded.last_opened_at",
            params![
                project.id.to_string(),
                project.root_path,
                project.name,
                project.git_remote,
                project.created_at.to_rfc3339(),
                project.last_opened_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn find_project_by_root(&self, root_path: &str) -> Result<Option<ProjectRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root_path, name, git_remote, created_at, last_opened_at
             FROM projects WHERE root_path = ?1",
        )?;
        let row = stmt
            .query_row(params![root_path], |r| {
                Ok(ProjectRecord {
                    id: parse_project_id(r.get::<_, String>(0)?)?,
                    root_path: r.get(1)?,
                    name: r.get(2)?,
                    git_remote: r.get(3)?,
                    created_at: parse_dt(r.get::<_, String>(4)?)?,
                    last_opened_at: parse_dt(r.get::<_, String>(5)?)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn list_recent_projects(&self, limit: usize) -> Result<Vec<ProjectRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root_path, name, git_remote, created_at, last_opened_at
             FROM projects ORDER BY last_opened_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(ProjectRecord {
                id: parse_project_id(r.get::<_, String>(0)?)?,
                root_path: r.get(1)?,
                name: r.get(2)?,
                git_remote: r.get(3)?,
                created_at: parse_dt(r.get::<_, String>(4)?)?,
                last_opened_at: parse_dt(r.get::<_, String>(5)?)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn insert_run(&self, run: &RunRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (
                id, project_id, objective, workflow_state, delivery_status, result_verdict,
                base_commit, worktree_path, test_intent_hash, config_snapshot_hash,
                pointers_json, snapshot_version, legacy_read_only, created_at, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                run.id.to_string(),
                run.project_id.to_string(),
                run.objective,
                serde_json::to_string(&run.workflow_state)?,
                serde_json::to_string(&run.delivery_status)?,
                run.result_verdict
                    .as_ref()
                    .and_then(|v| serde_json::to_string(v).ok()),
                run.base_commit,
                run.worktree_path,
                run.test_intent_hash,
                run.config_snapshot_hash,
                run.pointers_json,
                run.snapshot_version,
                run.legacy_read_only,
                run.created_at.to_rfc3339(),
                run.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_run(&self, id: RunId) -> Result<RunRecord> {
        self.conn
            .query_row(
                "SELECT id, project_id, objective, workflow_state, delivery_status, result_verdict,
                        base_commit, worktree_path, test_intent_hash, config_snapshot_hash,
                        pointers_json, snapshot_version, legacy_read_only, created_at, updated_at
                 FROM runs WHERE id = ?1",
                params![id.to_string()],
                map_run_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound(format!("run {id}")),
                other => StorageError::Sqlite(other),
            })
    }

    /// List runs newest first. Optional filters by project and coarse status bucket.
    pub fn list_runs(&self, project_id: Option<&str>, limit: usize) -> Result<Vec<RunRecord>> {
        let limit = limit as i64;
        if let Some(pid) = project_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, project_id, objective, workflow_state, delivery_status, result_verdict,
                        base_commit, worktree_path, test_intent_hash, config_snapshot_hash,
                        pointers_json, snapshot_version, legacy_read_only, created_at, updated_at
                 FROM runs WHERE project_id = ?1 ORDER BY updated_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![pid, limit], map_run_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            return Ok(out);
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, objective, workflow_state, delivery_status, result_verdict,
                    base_commit, worktree_path, test_intent_hash, config_snapshot_hash,
                    pointers_json, snapshot_version, legacy_read_only, created_at, updated_at
             FROM runs ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], map_run_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn update_run_objective(&self, id: RunId, objective: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE runs SET objective = ?1, updated_at = ?2 WHERE id = ?3",
            params![objective, now, id.to_string()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    pub fn set_run_snapshot<T: Serialize>(
        &self,
        id: RunId,
        test_intent_hash: &str,
        snapshot_hash: &str,
        snapshot: &T,
    ) -> Result<()> {
        let payload_json = serde_json::to_string(snapshot)?;
        if let Some((existing_hash, existing_payload)) = self
            .conn
            .query_row(
                "SELECT snapshot_hash, payload_json FROM run_snapshots WHERE run_id = ?1",
                params![id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if existing_hash == snapshot_hash && existing_payload == payload_json {
                return Ok(());
            }
            return Err(StorageError::Other(
                "run snapshot is immutable; create a successor run".into(),
            ));
        }
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO run_snapshots (run_id, snapshot_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                snapshot_hash,
                payload_json,
                Utc::now().to_rfc3339()
            ],
        )?;
        transaction.execute(
            "UPDATE runs SET test_intent_hash = ?1, snapshot_version = 1,
             legacy_read_only = 0, updated_at = ?2 WHERE id = ?3",
            params![test_intent_hash, Utc::now().to_rfc3339(), id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Insert an immutable snapshot while the caller already owns the Store's
    /// transaction boundary. This avoids a nested SQLite transaction and keeps
    /// Run creation + snapshot binding atomic.
    pub fn set_run_snapshot_in_current_transaction<T: Serialize>(
        &self,
        id: RunId,
        test_intent_hash: &str,
        snapshot_hash: &str,
        snapshot: &T,
    ) -> Result<()> {
        let payload_json = serde_json::to_string(snapshot)?;
        if self
            .conn
            .query_row(
                "SELECT 1 FROM run_snapshots WHERE run_id = ?1",
                params![id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(StorageError::Other(
                "run snapshot is immutable; create a successor run".into(),
            ));
        }
        self.conn.execute(
            "INSERT INTO run_snapshots (run_id, snapshot_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                snapshot_hash,
                payload_json,
                Utc::now().to_rfc3339()
            ],
        )?;
        self.conn.execute(
            "UPDATE runs SET test_intent_hash = ?1, snapshot_version = 1,
             legacy_read_only = 0, updated_at = ?2 WHERE id = ?3",
            params![test_intent_hash, Utc::now().to_rfc3339(), id.to_string()],
        )?;
        Ok(())
    }

    pub fn get_run_snapshot_json(&self, id: RunId) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT payload_json FROM run_snapshots WHERE run_id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_execution<T: Serialize>(
        &self,
        id: &str,
        run_id: RunId,
        revision_id: Option<&str>,
        purpose: &str,
        status: &str,
        scope_hash: &str,
        payload: &T,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO executions
             (id, run_id, revision_id, purpose, status, scope_hash, payload_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                id,
                run_id.to_string(),
                revision_id,
                purpose,
                status,
                scope_hash,
                serde_json::to_string(payload)?,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn finalize_execution<T: Serialize>(
        &self,
        id: &str,
        status: &str,
        payload: &T,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE executions SET status = ?1, payload_json = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                status,
                serde_json::to_string(payload)?,
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("execution {id}")));
        }
        Ok(())
    }

    pub fn list_execution_payloads(&self, run_id: RunId) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT payload_json FROM executions WHERE run_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map(params![run_id.to_string()], |row| row.get(0))?;
        let mut payloads = Vec::new();
        for row in rows {
            payloads.push(row?);
        }
        Ok(payloads)
    }

    pub fn list_execution_history(&self, run_id: RunId) -> Result<Vec<ExecutionHistoryRecord>> {
        let mut statement = self.conn.prepare(
            "SELECT id, purpose, status, scope_hash, payload_json
             FROM executions WHERE run_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map(params![run_id.to_string()], |row| {
            Ok(ExecutionHistoryRecord {
                id: row.get(0)?,
                purpose: row.get(1)?,
                status: row.get(2)?,
                scope_hash: row.get(3)?,
                payload_json: row.get(4)?,
            })
        })?;
        let mut history = Vec::new();
        for row in rows {
            history.push(row?);
        }
        Ok(history)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_profile_probe<T: Serialize>(
        &self,
        id: &str,
        project_id: ProjectId,
        base_commit: &str,
        profile_hash: &str,
        trust_scope_hash: &str,
        kind: &str,
        status: &str,
        payload: &T,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO profile_probes
             (id, project_id, base_commit, profile_hash, trust_scope_hash, kind, status, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                project_id.to_string(),
                base_commit,
                profile_hash,
                trust_scope_hash,
                kind,
                status,
                serde_json::to_string(payload)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn insert_test_revision<T: Serialize>(
        &self,
        id: &str,
        run_id: RunId,
        sequence: u32,
        candidate_tree: &str,
        scope_hash: &str,
        payload: &T,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO test_revisions
             (id, run_id, sequence, candidate_tree, scope_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                run_id.to_string(),
                sequence,
                candidate_tree,
                scope_hash,
                serde_json::to_string(payload)?,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn insert_process_intent<T: Serialize>(
        &self,
        id: &str,
        run_id: RunId,
        execution_id: Option<&str>,
        slot: &str,
        executable_identity: &str,
        payload: &T,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO process_tasks
             (id, run_id, execution_id, slot, status, executable_identity, payload_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'starting', ?5, ?6, ?7, ?7)",
            params![id, run_id.to_string(), execution_id, slot, executable_identity,
                serde_json::to_string(payload)?, now],
        )?;
        self.conn.execute(
            "INSERT INTO process_slots (task_id, run_id, slot, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, run_id.to_string(), slot, now],
        )?;
        Ok(())
    }

    pub fn ensure_process_capacity(&self, required_slots: usize) -> Result<()> {
        let active: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM process_slots", [], |row| row.get(0))?;
        if active.saturating_add(required_slots as i64) > 2 {
            return Err(StorageError::Other(
                "global process concurrency limit (2) reached across CLI and desktop".into(),
            ));
        }
        Ok(())
    }

    pub fn list_active_process_tasks(&self) -> Result<Vec<ActiveProcessTaskRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.run_id, p.slot, p.status, p.payload_json, p.created_at
             FROM process_tasks p
             INNER JOIN process_slots s ON s.task_id = p.id
             ORDER BY p.created_at, p.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ActiveProcessTaskRecord {
                id: row.get(0)?,
                run_id: parse_run_id(row.get::<_, String>(1)?)?,
                slot: row.get(2)?,
                status: row.get(3)?,
                payload_json: row.get(4)?,
                created_at: parse_dt(row.get::<_, String>(5)?)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn mark_active_process_tasks_for_run(&self, run_id: RunId, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE process_tasks SET status=?1, updated_at=?2
             WHERE run_id=?3 AND id IN (SELECT task_id FROM process_slots)",
            params![status, Utc::now().to_rfc3339(), run_id.to_string()],
        )?;
        self.conn.execute(
            "DELETE FROM process_slots WHERE run_id=?1",
            params![run_id.to_string()],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn acquire_activity_lease(
        &self,
        run_id: RunId,
        activity_kind: &str,
        owner_id: &str,
        owner_pid: u32,
        owner_start_token: &str,
        owner_executable_identity: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO activity_leases
                 (run_id, activity_kind, owner_id, owner_pid, owner_start_token,
                  owner_executable_identity, acquired_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run_id.to_string(),
                    activity_kind,
                    owner_id,
                    owner_pid,
                    owner_start_token,
                    owner_executable_identity,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    StorageError::Other(format!(
                        "run activity already owned by another CLI or desktop process: {run_id}"
                    ))
                }
                other => StorageError::Sqlite(other),
            })?;
        Ok(())
    }

    pub fn release_activity_lease(&self, run_id: RunId, owner_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM activity_leases WHERE run_id=?1 AND owner_id=?2",
            params![run_id.to_string(), owner_id],
        )?;
        Ok(())
    }

    pub fn list_activity_leases(&self) -> Result<Vec<ActivityLeaseRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, activity_kind, owner_id, owner_pid, owner_start_token,
                    owner_executable_identity, acquired_at
             FROM activity_leases ORDER BY acquired_at, run_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ActivityLeaseRecord {
                run_id: parse_run_id(row.get::<_, String>(0)?)?,
                activity_kind: row.get(1)?,
                owner_id: row.get(2)?,
                owner_pid: row.get(3)?,
                owner_start_token: row.get(4)?,
                owner_executable_identity: row.get(5)?,
                acquired_at: parse_dt(row.get::<_, String>(6)?)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn remove_activity_lease(&self, run_id: RunId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM activity_leases WHERE run_id=?1",
            params![run_id.to_string()],
        )?;
        Ok(())
    }

    pub fn insert_apply_intent<T: Serialize>(
        &self,
        id: &str,
        run_id: RunId,
        payload: &T,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO apply_intents (id, run_id, status, payload_json, created_at, updated_at)
             VALUES (?1, ?2, 'starting', ?3, ?4, ?4)",
            params![id, run_id.to_string(), serde_json::to_string(payload)?, now],
        )?;
        Ok(())
    }

    pub fn finalize_apply_intent<T: Serialize>(
        &self,
        id: &str,
        status: &str,
        payload: &T,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE apply_intents SET status=?1, payload_json=?2, updated_at=?3 WHERE id=?4",
            params![
                status,
                serde_json::to_string(payload)?,
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }

    pub fn set_process_identity<T: Serialize>(
        &self,
        id: &str,
        status: &str,
        pid: Option<u32>,
        pgid: Option<i32>,
        start_token: Option<&str>,
        payload: &T,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE process_tasks SET status=?1, pid=?2, pgid=?3, start_token=?4,
             payload_json=?5, updated_at=?6 WHERE id=?7",
            params![
                status,
                pid,
                pgid,
                start_token,
                serde_json::to_string(payload)?,
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("process task {id}")));
        }
        if !matches!(status, "starting" | "waiting_for_go" | "running") {
            self.conn
                .execute("DELETE FROM process_slots WHERE task_id=?1", params![id])?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_review<T: Serialize>(
        &self,
        id: &str,
        run_id: RunId,
        subject_type: &str,
        subject_id: &str,
        reviewer_type: &str,
        scope_hash: &str,
        payload: &T,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO reviews
             (id, run_id, subject_type, subject_id, reviewer_type, scope_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, run_id.to_string(), subject_type, subject_id, reviewer_type, scope_hash,
                serde_json::to_string(payload)?, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_decision<T: Serialize>(
        &self,
        id: &str,
        run_id: RunId,
        subject_type: &str,
        subject_id: &str,
        decision_type: &str,
        scope_hash: &str,
        payload: &T,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO decisions
             (id, run_id, subject_type, subject_id, decision_type, scope_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, run_id.to_string(), subject_type, subject_id, decision_type, scope_hash,
                serde_json::to_string(payload)?, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn list_audit_objects(&self, run_id: RunId) -> Result<Vec<serde_json::Value>> {
        let sql = "
            SELECT 'revision', id, payload_json, created_at FROM test_revisions WHERE run_id = ?1
            UNION ALL SELECT 'execution', id, payload_json, created_at FROM executions WHERE run_id = ?1
            UNION ALL SELECT 'review', id, payload_json, created_at FROM reviews WHERE run_id = ?1
            UNION ALL SELECT 'decision', id, payload_json, created_at FROM decisions WHERE run_id = ?1
            UNION ALL SELECT 'comment', id, json_object('body', body), created_at FROM comments WHERE run_id = ?1
            ORDER BY created_at ASC";
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(params![run_id.to_string()], |row| {
            let kind: String = row.get(0)?;
            let id: String = row.get(1)?;
            let payload: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            Ok(serde_json::json!({
                "kind": kind,
                "id": id,
                "payload": serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(serde_json::Value::Null),
                "created_at": created_at,
            }))
        })?;
        let mut values = Vec::new();
        for row in rows {
            values.push(row?);
        }
        Ok(values)
    }

    pub fn delete_run(&self, id: RunId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM events WHERE run_id = ?1",
            params![id.to_string()],
        )?;
        let n = self
            .conn
            .execute("DELETE FROM runs WHERE id = ?1", params![id.to_string()])?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    pub fn update_run_state(
        &self,
        id: RunId,
        state: WorkflowState,
        pointers: &RunPointers,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE runs SET workflow_state = ?1, pointers_json = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                serde_json::to_string(&state)?,
                serde_json::to_string(pointers)?,
                now,
                id.to_string()
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    pub fn update_run_worktree(
        &self,
        id: RunId,
        worktree_path: Option<&str>,
        state: WorkflowState,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE runs SET worktree_path = ?1, workflow_state = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                worktree_path,
                serde_json::to_string(&state)?,
                now,
                id.to_string()
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    pub fn update_run_completion(
        &self,
        id: RunId,
        state: WorkflowState,
        delivery: DeliveryStatus,
        verdict: Option<ResultVerdict>,
        worktree_path: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE runs SET workflow_state = ?1, delivery_status = ?2, result_verdict = ?3,
             worktree_path = COALESCE(?4, worktree_path), updated_at = ?5 WHERE id = ?6",
            params![
                serde_json::to_string(&state)?,
                serde_json::to_string(&delivery)?,
                verdict.as_ref().and_then(|v| serde_json::to_string(v).ok()),
                worktree_path,
                now,
                id.to_string(),
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!("run {id}")));
        }
        Ok(())
    }

    pub fn upsert_trust_grant(
        &self,
        id: &str,
        project_id: &str,
        trust_scope_hash: &str,
        payload_json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO trust_grants (id, project_id, trust_scope_hash, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
               trust_scope_hash = excluded.trust_scope_hash,
               payload_json = excluded.payload_json",
            params![
                id,
                project_id,
                trust_scope_hash,
                payload_json,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn find_trust_grant_payload(
        &self,
        project_id: &str,
        trust_scope_hash: &str,
    ) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT payload_json FROM trust_grants
                 WHERE project_id = ?1 AND trust_scope_hash = ?2
                 ORDER BY created_at DESC LIMIT 1",
                params![project_id, trust_scope_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn put_run_state<T: Serialize>(
        &self,
        run_id: RunId,
        state_key: &str,
        payload: &T,
    ) -> Result<()> {
        let payload_json = serde_json::to_string(payload)?;
        self.conn.execute(
            "INSERT INTO run_state (run_id, state_key, payload_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(run_id, state_key) DO UPDATE SET
               payload_json = excluded.payload_json,
               updated_at = excluded.updated_at",
            params![
                run_id.to_string(),
                state_key,
                payload_json,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn delete_run_state(&self, run_id: RunId, state_key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM run_state WHERE run_id = ?1 AND state_key = ?2",
            params![run_id.to_string(), state_key],
        )?;
        Ok(())
    }

    pub fn get_run_state_json(&self, run_id: RunId, state_key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT payload_json FROM run_state WHERE run_id=?1 AND state_key=?2",
                params![run_id.to_string(), state_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_run_state(&self) -> Result<Vec<RunStateRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, state_key, payload_json, updated_at
             FROM run_state ORDER BY run_id, state_key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RunStateRecord {
                run_id: parse_run_id(r.get::<_, String>(0)?)?,
                state_key: r.get(1)?,
                payload_json: r.get(2)?,
                updated_at: parse_dt(r.get::<_, String>(3)?)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn put_app_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let payload_json = serde_json::to_string(value)?;
        self.conn.execute(
            "INSERT INTO app_settings (setting_key, payload_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(setting_key) DO UPDATE SET
               payload_json = excluded.payload_json,
               updated_at = excluded.updated_at",
            params![key, payload_json, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_app_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT payload_json FROM app_settings WHERE setting_key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn get_project(&self, id: ProjectId) -> Result<ProjectRecord> {
        self.conn
            .query_row(
                "SELECT id, root_path, name, git_remote, created_at, last_opened_at
                 FROM projects WHERE id = ?1",
                params![id.to_string()],
                |r| {
                    Ok(ProjectRecord {
                        id: parse_project_id(r.get::<_, String>(0)?)?,
                        root_path: r.get(1)?,
                        name: r.get(2)?,
                        git_remote: r.get(3)?,
                        created_at: parse_dt(r.get::<_, String>(4)?)?,
                        last_opened_at: parse_dt(r.get::<_, String>(5)?)?,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("project {id}"))
                }
                other => StorageError::Sqlite(other),
            })
    }

    pub fn append_event(
        &self,
        run_id: Option<RunId>,
        project_id: Option<ProjectId>,
        event_type: &str,
        payload: &impl Serialize,
    ) -> Result<i64> {
        let payload_json = serde_json::to_string(payload)?;
        self.conn.execute(
            "INSERT INTO events (run_id, project_id, event_type, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id.map(|id| id.to_string()),
                project_id.map(|id| id.to_string()),
                event_type,
                payload_json,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_events_for_run(&self, run_id: RunId) -> Result<Vec<EventRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, project_id, event_type, payload_json, created_at
             FROM events WHERE run_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![run_id.to_string()], |r| {
            Ok(EventRecord {
                id: r.get(0)?,
                run_id: r
                    .get::<_, Option<String>>(1)?
                    .map(parse_run_id)
                    .transpose()?,
                project_id: r
                    .get::<_, Option<String>>(2)?
                    .map(parse_project_id)
                    .transpose()?,
                event_type: r.get(3)?,
                payload_json: r.get(4)?,
                created_at: parse_dt(r.get::<_, String>(5)?)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn map_run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let verdict: Option<String> = r.get(5)?;
    Ok(RunRecord {
        id: parse_run_id(r.get::<_, String>(0)?)?,
        project_id: parse_project_id(r.get::<_, String>(1)?)?,
        objective: r.get(2)?,
        workflow_state: serde_json::from_str(&r.get::<_, String>(3)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        delivery_status: serde_json::from_str(&r.get::<_, String>(4)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        result_verdict: match verdict {
            Some(s) => Some(
                serde_json::from_str(&s)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            ),
            None => None,
        },
        base_commit: r.get(6)?,
        worktree_path: r.get(7)?,
        test_intent_hash: r.get(8)?,
        config_snapshot_hash: r.get(9)?,
        pointers_json: r.get(10)?,
        snapshot_version: r.get::<_, Option<i64>>(11)?.map(|value| value as u32),
        legacy_read_only: r.get::<_, i64>(12)? != 0,
        created_at: parse_dt(r.get::<_, String>(13)?)?,
        updated_at: parse_dt(r.get::<_, String>(14)?)?,
    })
}

fn parse_dt(s: String) -> std::result::Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn parse_project_id(s: String) -> std::result::Result<ProjectId, rusqlite::Error> {
    Uuid::parse_str(&s)
        .map(ProjectId)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn parse_run_id(s: String) -> std::result::Result<RunId, rusqlite::Error> {
    Uuid::parse_str(&s)
        .map(RunId)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// Write bytes to content-addressed artifact store (hash path).
pub fn write_content_addressed(root: &Path, bytes: &[u8]) -> Result<(String, PathBuf)> {
    let mut writer = testharbor_evidence::ArtifactWriter::begin(root)
        .map_err(|error| StorageError::Other(error.to_string()))?;
    writer
        .write_chunk(bytes)
        .map_err(|error| StorageError::Other(error.to_string()))?;
    let artifact = writer
        .finalize()
        .map_err(|error| StorageError::Other(error.to_string()))?;
    Ok((artifact.content_sha256, artifact.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use testharbor_domain::RunPointers;

    fn setup_runs(root: &Path, count: usize) -> Vec<RunId> {
        let store = Store::open_at(root).unwrap();
        let now = Utc::now();
        let project = ProjectRecord {
            id: ProjectId::new(),
            root_path: format!("/tmp/testharbor-slots-{}", Uuid::new_v4()),
            name: "slots".into(),
            git_remote: None,
            created_at: now,
            last_opened_at: now,
        };
        store.upsert_project(&project).unwrap();
        (0..count)
            .map(|index| {
                let run = RunRecord {
                    id: RunId::new(),
                    project_id: project.id,
                    objective: format!("run-{index}"),
                    workflow_state: WorkflowState::Created,
                    delivery_status: DeliveryStatus::NotDelivered,
                    result_verdict: None,
                    base_commit: "abc123".into(),
                    worktree_path: None,
                    test_intent_hash: None,
                    config_snapshot_hash: None,
                    pointers_json: serde_json::to_string(&RunPointers::default()).unwrap(),
                    snapshot_version: Some(1),
                    legacy_read_only: false,
                    created_at: now,
                    updated_at: now,
                };
                store.insert_run(&run).unwrap();
                run.id
            })
            .collect()
    }

    fn race_for_slots(root: &Path, run_ids: Vec<RunId>) -> Vec<bool> {
        let barrier = Arc::new(Barrier::new(run_ids.len()));
        let handles = run_ids
            .into_iter()
            .enumerate()
            .map(|(index, run_id)| {
                let root = root.to_path_buf();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let store = Store::open_at(root).unwrap();
                    barrier.wait();
                    store
                        .transaction(|store| {
                            store.ensure_process_capacity(1)?;
                            store.insert_process_intent(
                                &format!("task-{index}"),
                                run_id,
                                None,
                                "foreground",
                                "sha256:test",
                                &serde_json::json!({"index": index}),
                            )
                        })
                        .is_ok()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    }

    #[test]
    fn project_and_run_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at(dir.path()).unwrap();
        let now = Utc::now();
        let project = ProjectRecord {
            id: ProjectId::new(),
            root_path: "/tmp/demo".into(),
            name: "demo".into(),
            git_remote: None,
            created_at: now,
            last_opened_at: now,
        };
        store.upsert_project(&project).unwrap();
        let found = store.find_project_by_root("/tmp/demo").unwrap().unwrap();
        assert_eq!(found.name, "demo");

        let run = RunRecord {
            id: RunId::new(),
            project_id: project.id,
            objective: "add login tests".into(),
            workflow_state: WorkflowState::Created,
            delivery_status: DeliveryStatus::NotDelivered,
            result_verdict: None,
            base_commit: "abc123".into(),
            worktree_path: None,
            test_intent_hash: None,
            config_snapshot_hash: None,
            pointers_json: serde_json::to_string(&RunPointers::default()).unwrap(),
            snapshot_version: None,
            legacy_read_only: false,
            created_at: now,
            updated_at: now,
        };
        store.insert_run(&run).unwrap();
        store
            .append_event(
                Some(run.id),
                Some(project.id),
                "run_created",
                &serde_json::json!({"base_commit": "abc123"}),
            )
            .unwrap();

        let loaded = store.get_run(run.id).unwrap();
        assert_eq!(loaded.objective, "add login tests");
        assert_eq!(loaded.workflow_state, WorkflowState::Created);

        let events = store.list_events_for_run(run.id).unwrap();
        store
            .set_run_snapshot(
                run.id,
                "intent-hash",
                "snapshot-hash",
                &serde_json::json!({"version": 1}),
            )
            .unwrap();
        store
            .set_run_snapshot(
                run.id,
                "intent-hash",
                "snapshot-hash",
                &serde_json::json!({"version": 1}),
            )
            .unwrap();
        let changed = store.set_run_snapshot(
            run.id,
            "different-intent",
            "different-snapshot",
            &serde_json::json!({"version": 2}),
        );
        assert!(changed.unwrap_err().to_string().contains("immutable"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "run_created");
    }

    #[test]
    fn content_addressed_stable() {
        let dir = tempfile::tempdir().unwrap();
        let (h1, p1) = write_content_addressed(dir.path(), b"hello").unwrap();
        let (h2, p2) = write_content_addressed(dir.path(), b"hello").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(p1, p2);
        assert_eq!(std::fs::read(p1).unwrap(), b"hello");
    }

    #[test]
    fn same_run_slot_is_atomic_across_connections() {
        let dir = tempfile::tempdir().unwrap();
        let run_id = setup_runs(dir.path(), 1)[0];
        let results = race_for_slots(dir.path(), vec![run_id, run_id]);
        assert_eq!(results.iter().filter(|success| **success).count(), 1);
        let store = Store::open_at(dir.path()).unwrap();
        assert_eq!(store.list_active_process_tasks().unwrap().len(), 1);
    }

    #[test]
    fn global_capacity_is_atomic_across_connections() {
        let dir = tempfile::tempdir().unwrap();
        let run_ids = setup_runs(dir.path(), 3);
        let results = race_for_slots(dir.path(), run_ids);
        assert_eq!(results.iter().filter(|success| **success).count(), 2);
        let store = Store::open_at(dir.path()).unwrap();
        let active = store.list_active_process_tasks().unwrap();
        assert_eq!(active.len(), 2);
        store
            .set_process_identity(
                &active[0].id,
                "exited",
                None,
                None,
                None,
                &serde_json::json!({"exit_code": 0}),
            )
            .unwrap();
        assert_eq!(store.list_active_process_tasks().unwrap().len(), 1);
        store.ensure_process_capacity(1).unwrap();
    }
}
