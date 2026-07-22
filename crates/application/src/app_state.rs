use crate::execution::{ActiveExecution, LastExecutionRecord};
use crate::generate::{GenerationRecovery, IntentMeta, PendingChangeRequest, TestEnvironmentInput};
use crate::history::RecoveryReport;
use crate::result_review::ResultDecisionRecord;
use crate::{AgentPreferences, UiPreferences};
use chrono::Utc;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use testharbor_agent_drivers::{AnalyzeResult, ResultReviewAgentResult};
use testharbor_domain::RunId;
use testharbor_process_supervisor::{
    current_process_identity, process_identity_matches, ProcessOutcome, ProcessSupervisor,
    SharedSupervisor,
};
use testharbor_result_normalizer::{
    IntendedTestManifest, NormalizedStructuredResult, TestInventory,
};
use testharbor_review::{CaseApprovalRecord, CaseReviewState};
use testharbor_storage::{DbPaths, StorageError, Store};
use testharbor_trust::TrustGrant;
use testharbor_workspace_manager::CandidateRevision;

pub type SharedAppState = Arc<AppState>;

pub struct AppState {
    store: Mutex<Store>,
    data_dir: PathBuf,
    supervisor: SharedSupervisor,
    trust_by_run: Mutex<HashMap<String, TrustGrant>>,
    active_exec: Mutex<HashMap<String, ActiveExecution>>,
    inventory_by_run: Mutex<HashMap<String, TestInventory>>,
    intended_by_run: Mutex<HashMap<String, IntendedTestManifest>>,
    structured_by_run: Mutex<HashMap<String, NormalizedStructuredResult>>,
    candidate_by_run: Mutex<HashMap<String, CandidateRevision>>,
    case_review_by_run: Mutex<HashMap<String, CaseReviewState>>,
    case_scope_by_run: Mutex<HashMap<String, String>>,
    case_approval_by_run: Mutex<HashMap<String, CaseApprovalRecord>>,
    intent_meta_by_run: Mutex<HashMap<String, IntentMeta>>,
    codex_generation_session_by_run: Mutex<HashMap<String, String>>,
    revision_seq_by_run: Mutex<HashMap<String, u32>>,
    result_decision_by_run: Mutex<HashMap<String, ResultDecisionRecord>>,
    analysis_by_run: Mutex<HashMap<String, AnalyzeResult>>,
    result_agent_review_by_run: Mutex<HashMap<String, ResultReviewAgentResult>>,
    last_execution_by_run: Mutex<HashMap<String, LastExecutionRecord>>,
    pending_change_by_run: Mutex<HashMap<String, PendingChangeRequest>>,
    generation_recovery_by_run: Mutex<HashMap<String, GenerationRecovery>>,
    test_environment_by_run: Mutex<HashMap<String, TestEnvironmentInput>>,
    last_recovery: Mutex<Option<RecoveryReport>>,
    ui_preferences: Mutex<UiPreferences>,
    agent_preferences: Mutex<AgentPreferences>,
}

impl AppState {
    pub fn open_default() -> Result<Self, StorageError> {
        let store = Store::open_default()?;
        let data_dir = store.paths().root.clone();
        let state = Self::from_store(store, data_dir);
        state.hydrate_persisted_state()?;
        let _ = state.reconcile_process_slots();
        let _ = state.reconcile_activity_leases();
        // Best-effort recovery; ignore errors at boot.
        let _ = crate::history::recover_on_startup(&state);
        Ok(state)
    }

    pub fn open_at(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        let store = Store::open_at(&root)?;
        let state = Self::from_store(store, root);
        state.hydrate_persisted_state()?;
        let _ = state.reconcile_process_slots();
        let _ = state.reconcile_activity_leases();
        let _ = crate::history::recover_on_startup(&state);
        Ok(state)
    }

    fn from_store(store: Store, data_dir: PathBuf) -> Self {
        Self {
            store: Mutex::new(store),
            data_dir,
            supervisor: Arc::new(ProcessSupervisor::new()),
            trust_by_run: Mutex::new(HashMap::new()),
            active_exec: Mutex::new(HashMap::new()),
            inventory_by_run: Mutex::new(HashMap::new()),
            intended_by_run: Mutex::new(HashMap::new()),
            structured_by_run: Mutex::new(HashMap::new()),
            candidate_by_run: Mutex::new(HashMap::new()),
            case_review_by_run: Mutex::new(HashMap::new()),
            case_scope_by_run: Mutex::new(HashMap::new()),
            case_approval_by_run: Mutex::new(HashMap::new()),
            intent_meta_by_run: Mutex::new(HashMap::new()),
            codex_generation_session_by_run: Mutex::new(HashMap::new()),
            revision_seq_by_run: Mutex::new(HashMap::new()),
            result_decision_by_run: Mutex::new(HashMap::new()),
            analysis_by_run: Mutex::new(HashMap::new()),
            result_agent_review_by_run: Mutex::new(HashMap::new()),
            last_execution_by_run: Mutex::new(HashMap::new()),
            pending_change_by_run: Mutex::new(HashMap::new()),
            generation_recovery_by_run: Mutex::new(HashMap::new()),
            test_environment_by_run: Mutex::new(HashMap::new()),
            last_recovery: Mutex::new(None),
            ui_preferences: Mutex::new(UiPreferences::default()),
            agent_preferences: Mutex::new(AgentPreferences::default()),
        }
    }

    fn hydrate_persisted_state(&self) -> Result<(), StorageError> {
        if let Some(payload) = self.with_store(|store| store.get_app_setting("ui_preferences"))? {
            if let Ok(preferences) = serde_json::from_str::<UiPreferences>(&payload) {
                *self.ui_preferences.lock() = UiPreferences {
                    language: normalize_ui_language(&preferences.language).into(),
                };
            }
        }
        if let Some(payload) =
            self.with_store(|store| store.get_app_setting("agent_preferences"))?
        {
            if let Ok(preferences) = serde_json::from_str::<AgentPreferences>(&payload) {
                *self.agent_preferences.lock() = preferences;
            }
        }
        let rows = self.with_store(|store| store.list_run_state())?;
        for row in rows {
            let run_id = row.run_id.to_string();
            match row.state_key.as_str() {
                "trust_grant" => {
                    self.trust_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "active_execution" => {
                    let execution: ActiveExecution = serde_json::from_str(&row.payload_json)?;
                    if self
                        .supervisor
                        .attach(&execution.task_id, execution.outcome.clone())
                        .is_ok()
                    {
                        self.active_exec.lock().insert(run_id, execution);
                    }
                }
                "inventory" => {
                    self.inventory_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "intended" => {
                    self.intended_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "structured" => {
                    self.structured_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "candidate" => {
                    self.candidate_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "case_review" => {
                    self.case_review_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "case_scope" => {
                    self.case_scope_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "case_approval" => {
                    self.case_approval_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "intent_meta" => {
                    self.intent_meta_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "codex_generation_session" => {
                    self.codex_generation_session_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "revision_seq" => {
                    self.revision_seq_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "result_decision" => {
                    self.result_decision_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "analysis" => {
                    self.analysis_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "result_agent_review" => {
                    self.result_agent_review_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "last_execution" => {
                    self.last_execution_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "pending_change" => {
                    self.pending_change_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                "generation_recovery" => {
                    self.generation_recovery_by_run
                        .lock()
                        .insert(run_id, serde_json::from_str(&row.payload_json)?);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn persist<T: serde::Serialize>(
        &self,
        run_id: &str,
        key: &str,
        value: &T,
    ) -> Result<(), StorageError> {
        let id = uuid::Uuid::parse_str(run_id)
            .map(RunId)
            .map_err(|_| StorageError::Other(format!("invalid run id: {run_id}")))?;
        self.with_store(|store| store.put_run_state(id, key, value))
    }

    fn delete_persisted(&self, run_id: &str, key: &str) -> Result<(), StorageError> {
        let id = uuid::Uuid::parse_str(run_id)
            .map(RunId)
            .map_err(|_| StorageError::Other(format!("invalid run id: {run_id}")))?;
        self.with_store(|store| store.delete_run_state(id, key))
    }

    pub fn with_store<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&Store) -> T,
    {
        let guard = self.store.lock();
        f(&guard)
    }

    pub fn with_store_transaction<T>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let guard = self.store.lock();
        guard.transaction(operation)
    }

    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    pub fn paths(&self) -> DbPaths {
        self.with_store(|s| s.paths().clone())
    }

    pub fn supervisor(&self) -> &ProcessSupervisor {
        &self.supervisor
    }

    pub fn set_trust_grant(&self, run_id: String, grant: TrustGrant) -> Result<(), StorageError> {
        self.persist(&run_id, "trust_grant", &grant)?;
        self.trust_by_run.lock().insert(run_id, grant);
        Ok(())
    }

    pub fn get_trust_grant(&self, run_id: &str) -> Option<TrustGrant> {
        self.trust_by_run.lock().get(run_id).cloned()
    }

    pub fn set_active_execution(
        &self,
        run_id: String,
        exec: ActiveExecution,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "active_execution", &exec)?;
        self.active_exec.lock().insert(run_id, exec);
        Ok(())
    }

    pub fn get_active_execution(&self, run_id: &str) -> Option<ActiveExecution> {
        let id = uuid::Uuid::parse_str(run_id).ok().map(RunId)?;
        let payload = self
            .with_store(|store| store.get_run_state_json(id, "active_execution"))
            .ok()
            .flatten();
        let Some(payload) = payload else {
            self.active_exec.lock().remove(run_id);
            return None;
        };
        let execution: ActiveExecution = serde_json::from_str(&payload).ok()?;
        if !self.supervisor.has_task(&execution.task_id) {
            if self
                .supervisor
                .attach(&execution.task_id, execution.outcome.clone())
                .is_err()
            {
                self.active_exec.lock().remove(run_id);
                return None;
            }
        }
        self.active_exec
            .lock()
            .insert(run_id.to_string(), execution.clone());
        Some(execution)
    }

    /// Synchronize DB-owned process slots into this process's Supervisor.
    /// Invalid/stale identities are failed closed and release their DB slot.
    pub fn reconcile_process_slots(&self) -> Result<(), StorageError> {
        let tasks = self.with_store(|store| store.list_active_process_tasks())?;
        for task in tasks {
            if self.supervisor.has_task(&task.id) {
                continue;
            }
            if task.status == "starting"
                && Utc::now()
                    .signed_duration_since(task.created_at)
                    .num_seconds()
                    < 30
            {
                continue;
            }
            let outcome = serde_json::from_str::<ProcessOutcome>(&task.payload_json);
            let attached = outcome
                .as_ref()
                .ok()
                .is_some_and(|outcome| self.supervisor.attach(&task.id, outcome.clone()).is_ok());
            if !attached {
                self.with_store(|store| {
                    store.set_process_identity(
                        &task.id,
                        "lost",
                        None,
                        None,
                        None,
                        &serde_json::json!({
                            "reason": "stale or mismatched cross-process identity"
                        }),
                    )
                })?;
            }
        }
        Ok(())
    }

    pub fn acquire_run_activity(
        &self,
        run_id: RunId,
        activity_kind: &str,
    ) -> Result<String, StorageError> {
        let (pid, start_token, executable_identity) =
            current_process_identity().map_err(|error| StorageError::Other(error.to_string()))?;
        let owner_id = uuid::Uuid::new_v4().to_string();
        self.with_store(|store| {
            store.acquire_activity_lease(
                run_id,
                activity_kind,
                &owner_id,
                pid,
                &start_token,
                &executable_identity,
            )
        })?;
        Ok(owner_id)
    }

    pub fn release_run_activity(&self, run_id: RunId, owner_id: &str) -> Result<(), StorageError> {
        self.with_store(|store| store.release_activity_lease(run_id, owner_id))
    }

    pub fn has_live_run_activity(&self, run_id: RunId, activity_kind: &str) -> bool {
        self.with_store(|store| store.list_activity_leases())
            .ok()
            .into_iter()
            .flatten()
            .any(|lease| {
                lease.run_id == run_id
                    && lease.activity_kind == activity_kind
                    && process_identity_matches(
                        lease.owner_pid,
                        &lease.owner_start_token,
                        &lease.owner_executable_identity,
                    )
            })
    }

    pub fn reconcile_activity_leases(&self) -> Result<(), StorageError> {
        let leases = self.with_store(|store| store.list_activity_leases())?;
        for lease in leases {
            if !process_identity_matches(
                lease.owner_pid,
                &lease.owner_start_token,
                &lease.owner_executable_identity,
            ) {
                self.with_store(|store| store.remove_activity_lease(lease.run_id))?;
            }
        }
        Ok(())
    }

    pub fn take_active_execution(&self, run_id: &str) -> Option<ActiveExecution> {
        self.active_exec.lock().remove(run_id)
    }

    pub fn clear_active_execution(&self, run_id: &str) -> Result<(), StorageError> {
        self.active_exec.lock().remove(run_id);
        self.delete_persisted(run_id, "active_execution")
    }

    pub fn set_inventory(&self, run_id: String, inv: TestInventory) -> Result<(), StorageError> {
        self.persist(&run_id, "inventory", &inv)?;
        self.inventory_by_run.lock().insert(run_id, inv);
        Ok(())
    }

    pub fn get_inventory(&self, run_id: &str) -> Option<TestInventory> {
        self.inventory_by_run.lock().get(run_id).cloned()
    }

    pub fn set_intended(
        &self,
        run_id: String,
        m: IntendedTestManifest,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "intended", &m)?;
        self.intended_by_run.lock().insert(run_id, m);
        Ok(())
    }

    pub fn get_intended(&self, run_id: &str) -> Option<IntendedTestManifest> {
        self.intended_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_intended(&self, run_id: &str) -> Result<(), StorageError> {
        self.intended_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "intended")
    }

    pub fn set_structured(
        &self,
        run_id: String,
        r: NormalizedStructuredResult,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "structured", &r)?;
        self.structured_by_run.lock().insert(run_id, r);
        Ok(())
    }

    pub fn get_structured(&self, run_id: &str) -> Option<NormalizedStructuredResult> {
        self.structured_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_structured(&self, run_id: &str) -> Result<(), StorageError> {
        self.structured_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "structured")
    }

    pub fn set_candidate(&self, run_id: String, c: CandidateRevision) -> Result<(), StorageError> {
        self.persist(&run_id, "candidate", &c)?;
        self.candidate_by_run.lock().insert(run_id, c);
        Ok(())
    }

    pub fn get_candidate(&self, run_id: &str) -> Option<CandidateRevision> {
        self.candidate_by_run.lock().get(run_id).cloned()
    }

    pub fn set_case_review(&self, run_id: String, r: CaseReviewState) -> Result<(), StorageError> {
        self.persist(&run_id, "case_review", &r)?;
        self.case_review_by_run.lock().insert(run_id, r);
        Ok(())
    }

    pub fn get_case_review(&self, run_id: &str) -> Option<CaseReviewState> {
        self.case_review_by_run.lock().get(run_id).cloned()
    }

    pub fn set_case_scope(&self, run_id: String, scope: String) -> Result<(), StorageError> {
        self.persist(&run_id, "case_scope", &scope)?;
        self.case_scope_by_run.lock().insert(run_id, scope);
        Ok(())
    }

    pub fn get_case_scope(&self, run_id: &str) -> Option<String> {
        self.case_scope_by_run.lock().get(run_id).cloned()
    }

    pub fn set_case_approval(
        &self,
        run_id: String,
        a: CaseApprovalRecord,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "case_approval", &a)?;
        self.case_approval_by_run.lock().insert(run_id, a);
        Ok(())
    }

    pub fn get_case_approval(&self, run_id: &str) -> Option<CaseApprovalRecord> {
        self.case_approval_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_case_approval(&self, run_id: &str) -> Result<(), StorageError> {
        self.case_approval_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "case_approval")
    }

    pub fn set_intent_meta(&self, run_id: String, m: IntentMeta) -> Result<(), StorageError> {
        self.persist(&run_id, "intent_meta", &m)?;
        self.intent_meta_by_run.lock().insert(run_id, m);
        Ok(())
    }

    pub fn get_intent_meta(&self, run_id: &str) -> Option<IntentMeta> {
        self.intent_meta_by_run.lock().get(run_id).cloned()
    }

    pub fn set_test_environment(&self, run_id: String, environment: TestEnvironmentInput) {
        self.test_environment_by_run
            .lock()
            .insert(run_id, environment);
    }

    pub fn get_test_environment(&self, run_id: &str) -> Option<TestEnvironmentInput> {
        if let Some(environment) = self.test_environment_by_run.lock().get(run_id).cloned() {
            return Some(environment);
        }
        let persisted = self.get_intent_meta(run_id)?.environment?;
        Some(TestEnvironmentInput {
            context: persisted.context,
            base_url: persisted.base_url,
            health_url: persisted.health_url,
            login_url: persisted.login_url,
            startup_command: persisted.startup_command,
            startup_cwd: persisted.startup_cwd,
            username: None,
            password: None,
            variables: HashMap::new(),
        })
    }

    pub fn set_codex_generation_session(
        &self,
        run_id: String,
        session_id: String,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "codex_generation_session", &session_id)?;
        self.codex_generation_session_by_run
            .lock()
            .insert(run_id, session_id);
        Ok(())
    }

    pub fn get_codex_generation_session(&self, run_id: &str) -> Option<String> {
        self.codex_generation_session_by_run
            .lock()
            .get(run_id)
            .cloned()
    }

    pub fn clear_codex_generation_session(&self, run_id: &str) -> Result<(), StorageError> {
        self.codex_generation_session_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "codex_generation_session")
    }

    pub fn next_revision_seq(&self, run_id: &str) -> Result<u32, StorageError> {
        let mut m = self.revision_seq_by_run.lock();
        let e = m.entry(run_id.to_string()).or_insert(0);
        *e += 1;
        let value = *e;
        drop(m);
        self.persist(run_id, "revision_seq", &value)?;
        Ok(value)
    }

    pub fn set_result_decision(
        &self,
        run_id: String,
        d: ResultDecisionRecord,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "result_decision", &d)?;
        self.result_decision_by_run.lock().insert(run_id, d);
        Ok(())
    }

    pub fn get_result_decision(&self, run_id: &str) -> Option<ResultDecisionRecord> {
        self.result_decision_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_result_decision(&self, run_id: &str) -> Result<(), StorageError> {
        self.result_decision_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "result_decision")
    }

    pub fn set_analysis(&self, run_id: String, a: AnalyzeResult) -> Result<(), StorageError> {
        self.persist(&run_id, "analysis", &a)?;
        self.analysis_by_run.lock().insert(run_id, a);
        Ok(())
    }

    pub fn get_analysis(&self, run_id: &str) -> Option<AnalyzeResult> {
        self.analysis_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_analysis(&self, run_id: &str) -> Result<(), StorageError> {
        self.analysis_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "analysis")
    }

    pub fn set_result_agent_review(
        &self,
        run_id: String,
        r: ResultReviewAgentResult,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "result_agent_review", &r)?;
        self.result_agent_review_by_run.lock().insert(run_id, r);
        Ok(())
    }

    pub fn get_result_agent_review(&self, run_id: &str) -> Option<ResultReviewAgentResult> {
        self.result_agent_review_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_result_agent_review(&self, run_id: &str) -> Result<(), StorageError> {
        self.result_agent_review_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "result_agent_review")
    }

    pub fn set_last_execution(
        &self,
        run_id: String,
        execution: LastExecutionRecord,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "last_execution", &execution)?;
        self.last_execution_by_run.lock().insert(run_id, execution);
        Ok(())
    }

    pub fn get_last_execution(&self, run_id: &str) -> Option<LastExecutionRecord> {
        self.last_execution_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_last_execution(&self, run_id: &str) -> Result<(), StorageError> {
        self.last_execution_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "last_execution")
    }

    pub fn set_pending_change(
        &self,
        run_id: String,
        request: PendingChangeRequest,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "pending_change", &request)?;
        self.pending_change_by_run.lock().insert(run_id, request);
        Ok(())
    }

    pub fn get_pending_change(&self, run_id: &str) -> Option<PendingChangeRequest> {
        self.pending_change_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_pending_change(&self, run_id: &str) -> Result<(), StorageError> {
        self.pending_change_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "pending_change")
    }

    pub fn set_generation_recovery(
        &self,
        run_id: String,
        recovery: GenerationRecovery,
    ) -> Result<(), StorageError> {
        self.persist(&run_id, "generation_recovery", &recovery)?;
        self.generation_recovery_by_run
            .lock()
            .insert(run_id, recovery);
        Ok(())
    }

    pub fn get_generation_recovery(&self, run_id: &str) -> Option<GenerationRecovery> {
        self.generation_recovery_by_run.lock().get(run_id).cloned()
    }

    pub fn clear_generation_recovery(&self, run_id: &str) -> Result<(), StorageError> {
        self.generation_recovery_by_run.lock().remove(run_id);
        self.test_environment_by_run.lock().remove(run_id);
        self.delete_persisted(run_id, "generation_recovery")
    }

    pub fn clear_run_cache(&self, run_id: &str) {
        self.trust_by_run.lock().remove(run_id);
        self.active_exec.lock().remove(run_id);
        self.inventory_by_run.lock().remove(run_id);
        self.intended_by_run.lock().remove(run_id);
        self.structured_by_run.lock().remove(run_id);
        self.candidate_by_run.lock().remove(run_id);
        self.case_review_by_run.lock().remove(run_id);
        self.case_scope_by_run.lock().remove(run_id);
        self.case_approval_by_run.lock().remove(run_id);
        self.intent_meta_by_run.lock().remove(run_id);
        self.codex_generation_session_by_run.lock().remove(run_id);
        self.revision_seq_by_run.lock().remove(run_id);
        self.result_decision_by_run.lock().remove(run_id);
        self.analysis_by_run.lock().remove(run_id);
        self.result_agent_review_by_run.lock().remove(run_id);
        self.last_execution_by_run.lock().remove(run_id);
        self.pending_change_by_run.lock().remove(run_id);
        self.generation_recovery_by_run.lock().remove(run_id);
    }

    pub fn set_last_recovery(&self, report: RecoveryReport) {
        *self.last_recovery.lock() = Some(report);
    }

    pub fn get_last_recovery(&self) -> Option<RecoveryReport> {
        self.last_recovery.lock().clone()
    }

    pub fn get_agent_preferences(&self) -> AgentPreferences {
        self.agent_preferences.lock().clone()
    }

    pub fn get_ui_preferences(&self) -> UiPreferences {
        self.ui_preferences.lock().clone()
    }

    pub fn set_ui_language(&self, language: &str) -> Result<UiPreferences, StorageError> {
        let preferences = UiPreferences {
            language: normalize_ui_language(language).into(),
        };
        self.with_store(|store| store.put_app_setting("ui_preferences", &preferences))?;
        *self.ui_preferences.lock() = preferences.clone();
        Ok(preferences)
    }

    pub fn set_agent_preferences(
        &self,
        enabled_modes: Vec<String>,
    ) -> Result<AgentPreferences, StorageError> {
        let mut normalized = Vec::new();
        for mode in enabled_modes {
            let mode = mode.to_lowercase();
            if ["codex", "claude", "grok"].contains(&mode.as_str()) && !normalized.contains(&mode) {
                normalized.push(mode);
            }
        }
        let preferences = AgentPreferences {
            enabled_modes: normalized,
        };
        self.with_store(|store| store.put_app_setting("agent_preferences", &preferences))?;
        *self.agent_preferences.lock() = preferences.clone();
        Ok(preferences)
    }
}

fn normalize_ui_language(language: &str) -> &'static str {
    match language.trim().to_ascii_lowercase().as_str() {
        "zh" | "zh-cn" | "zh_cn" | "chinese" => "zh-CN",
        _ => "en",
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::execution::ExecutionPurpose;
    use chrono::Utc;
    use testharbor_domain::{DeliveryStatus, ProjectId, RunPointers, WorkflowState};
    use testharbor_process_supervisor::{CommandSpec, TaskPhase};
    use testharbor_storage::{ProjectRecord, RunRecord};

    #[test]
    fn ui_language_defaults_to_english_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open_at(dir.path()).unwrap();
        assert_eq!(state.get_ui_preferences().language, "en");
        assert_eq!(state.set_ui_language("zh_cn").unwrap().language, "zh-CN");
        drop(state);

        let reopened = AppState::open_at(dir.path()).unwrap();
        assert_eq!(reopened.get_ui_preferences().language, "zh-CN");
        assert_eq!(
            reopened.set_ui_language("unsupported").unwrap().language,
            "en"
        );
    }

    #[test]
    fn already_open_state_discovers_execution_started_by_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let owner = AppState::open_at(dir.path()).unwrap();
        let now = Utc::now();
        let project = ProjectRecord {
            id: ProjectId::new(),
            root_path: dir.path().join("repo").display().to_string(),
            name: "cross-process".into(),
            git_remote: None,
            created_at: now,
            last_opened_at: now,
        };
        std::fs::create_dir_all(&project.root_path).unwrap();
        owner
            .with_store(|store| store.upsert_project(&project))
            .unwrap();
        let run = RunRecord {
            id: RunId::new(),
            project_id: project.id,
            objective: "cross-process".into(),
            workflow_state: WorkflowState::Created,
            delivery_status: DeliveryStatus::NotDelivered,
            result_verdict: None,
            base_commit: "base".into(),
            worktree_path: None,
            test_intent_hash: None,
            config_snapshot_hash: None,
            pointers_json: serde_json::to_string(&RunPointers::default()).unwrap(),
            snapshot_version: Some(1),
            legacy_read_only: false,
            created_at: now,
            updated_at: now,
        };
        owner.with_store(|store| store.insert_run(&run)).unwrap();

        // Open before the owner starts the process to reproduce a long-lived
        // desktop app observing a later CLI execution.
        let observer = AppState::open_at(dir.path()).unwrap();
        let task_id = "cross-process-task";
        let outcome = owner
            .supervisor()
            .start(
                task_id,
                &CommandSpec {
                    executable: "/bin/sleep".into(),
                    args: vec!["30".into()],
                    cwd: dir.path().to_path_buf(),
                    env: HashMap::new(),
                    timeout_ms: Some(60_000),
                },
                &dir.path().join("logs"),
            )
            .unwrap();
        owner
            .set_active_execution(
                run.id.to_string(),
                ActiveExecution {
                    execution_id: "execution-1".into(),
                    task_id: task_id.into(),
                    parser: "exit-code".into(),
                    outcome,
                    purpose: ExecutionPurpose::Formal,
                    service_task_id: None,
                },
            )
            .unwrap();

        let discovered = observer.get_active_execution(&run.id.to_string()).unwrap();
        assert_eq!(discovered.task_id, task_id);
        assert!(observer.supervisor().has_task(task_id));
        assert_eq!(
            observer.supervisor().cancel(task_id).unwrap().phase,
            TaskPhase::Cancelled
        );
    }

    #[test]
    fn live_generation_lease_prevents_cross_process_recovery_and_double_start() {
        let dir = tempfile::tempdir().unwrap();
        let owner = AppState::open_at(dir.path()).unwrap();
        let now = Utc::now();
        let project = ProjectRecord {
            id: ProjectId::new(),
            root_path: dir.path().join("repo-generation").display().to_string(),
            name: "generation-lease".into(),
            git_remote: None,
            created_at: now,
            last_opened_at: now,
        };
        std::fs::create_dir_all(&project.root_path).unwrap();
        owner
            .with_store(|store| store.upsert_project(&project))
            .unwrap();
        let run = RunRecord {
            id: RunId::new(),
            project_id: project.id,
            objective: "generation".into(),
            workflow_state: WorkflowState::GeneratingTests,
            delivery_status: DeliveryStatus::NotDelivered,
            result_verdict: None,
            base_commit: "base".into(),
            worktree_path: None,
            test_intent_hash: None,
            config_snapshot_hash: None,
            pointers_json: serde_json::to_string(&RunPointers::default()).unwrap(),
            snapshot_version: Some(1),
            legacy_read_only: false,
            created_at: now,
            updated_at: now,
        };
        owner.with_store(|store| store.insert_run(&run)).unwrap();
        let owner_id = owner.acquire_run_activity(run.id, "generation").unwrap();

        let observer = AppState::open_at(dir.path()).unwrap();
        assert_eq!(
            observer
                .with_store(|store| store.get_run(run.id))
                .unwrap()
                .workflow_state,
            WorkflowState::GeneratingTests
        );
        assert!(observer
            .acquire_run_activity(run.id, "generation")
            .unwrap_err()
            .to_string()
            .contains("already owned"));
        owner.release_run_activity(run.id, &owner_id).unwrap();
    }
}
