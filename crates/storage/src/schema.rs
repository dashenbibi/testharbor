use rusqlite::Connection;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            git_remote TEXT,
            created_at TEXT NOT NULL,
            last_opened_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS runs (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id),
            objective TEXT NOT NULL,
            workflow_state TEXT NOT NULL,
            delivery_status TEXT NOT NULL,
            result_verdict TEXT,
            base_commit TEXT NOT NULL,
            worktree_path TEXT,
            test_intent_hash TEXT,
            config_snapshot_hash TEXT,
            pointers_json TEXT NOT NULL,
            snapshot_version INTEGER,
            legacy_read_only INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_runs_project ON runs(project_id);
        CREATE INDEX IF NOT EXISTS idx_runs_updated ON runs(updated_at);

        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT,
            project_id TEXT,
            event_type TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_events_run ON events(run_id);

        CREATE TABLE IF NOT EXISTS artifacts (
            id TEXT PRIMARY KEY,
            content_sha256 TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            media_type TEXT,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS trust_grants (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            trust_scope_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS run_state (
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            state_key TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (run_id, state_key)
        );

        CREATE INDEX IF NOT EXISTS idx_run_state_run ON run_state(run_id);

        CREATE TABLE IF NOT EXISTS app_settings (
            setting_key TEXT PRIMARY KEY,
            payload_json TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS run_snapshots (
            run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
            snapshot_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS test_revisions (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            candidate_tree TEXT NOT NULL,
            scope_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(run_id, sequence)
        );

        CREATE TABLE IF NOT EXISTS executions (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            revision_id TEXT,
            purpose TEXT NOT NULL,
            status TEXT NOT NULL,
            scope_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS reviews (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            subject_type TEXT NOT NULL,
            subject_id TEXT NOT NULL,
            reviewer_type TEXT NOT NULL,
            scope_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS decisions (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            subject_type TEXT NOT NULL,
            subject_id TEXT NOT NULL,
            decision_type TEXT NOT NULL,
            scope_hash TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS comments (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            subject_type TEXT NOT NULL,
            subject_id TEXT NOT NULL,
            body TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS process_tasks (
            id TEXT PRIMARY KEY,
            run_id TEXT REFERENCES runs(id) ON DELETE CASCADE,
            execution_id TEXT,
            slot TEXT NOT NULL,
            status TEXT NOT NULL,
            executable_identity TEXT NOT NULL,
            pid INTEGER,
            pgid INTEGER,
            start_token TEXT,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS process_slots (
            task_id TEXT PRIMARY KEY REFERENCES process_tasks(id) ON DELETE CASCADE,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            slot TEXT NOT NULL,
            acquired_at TEXT NOT NULL,
            UNIQUE(run_id, slot)
        );

        CREATE TABLE IF NOT EXISTS activity_leases (
            run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
            activity_kind TEXT NOT NULL,
            owner_id TEXT NOT NULL,
            owner_pid INTEGER NOT NULL,
            owner_start_token TEXT NOT NULL,
            owner_executable_identity TEXT NOT NULL,
            acquired_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS profile_probes (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES projects(id),
            base_commit TEXT NOT NULL,
            profile_hash TEXT NOT NULL,
            trust_scope_hash TEXT NOT NULL,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS apply_intents (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;

    let columns = conn
        .prepare("PRAGMA table_info(runs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let added_snapshot_version = !columns.iter().any(|column| column == "snapshot_version");
    let added_legacy_flag = !columns.iter().any(|column| column == "legacy_read_only");
    if added_snapshot_version {
        conn.execute("ALTER TABLE runs ADD COLUMN snapshot_version INTEGER", [])?;
    }
    if added_legacy_flag {
        conn.execute(
            "ALTER TABLE runs ADD COLUMN legacy_read_only INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if added_snapshot_version || added_legacy_flag {
        conn.execute("UPDATE runs SET legacy_read_only = 1", [])?;
    }

    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))?;
    if count == 0 {
        conn.execute("INSERT INTO schema_version (version) VALUES (4)", [])?;
    } else {
        conn.execute("UPDATE schema_version SET version = 4", [])?;
    }
    Ok(())
}
