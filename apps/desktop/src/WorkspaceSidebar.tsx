import { useEffect, useMemo, useState } from "react";
import { translate, type Language } from "./i18n";

export type SidebarProject = {
  id: string;
  root_path: string;
  name: string;
};

export type SidebarRun = {
  run_id: string;
  project_id: string;
  objective: string;
  workflow_state: string;
  status_bucket: string;
  updated_at: string;
  legacy_read_only?: boolean;
};

type WorkspaceSidebarProps = {
  projects: SidebarProject[];
  runs: SidebarRun[];
  activeProjectId: string | null;
  activeRunId: string | null;
  busy: boolean;
  onSelectProject: (project: SidebarProject) => void;
  onSelectRun: (run: SidebarRun) => void;
  onCreateRun: (project: SidebarProject | null) => void;
  onDeleteRun: (run: SidebarRun) => void;
  onCopyRunInputs: (run: SidebarRun) => void;
  onAddProject: () => void;
  onOpenSettings: () => void;
  serviceReady: boolean;
  sidebarCollapsed: boolean;
  onToggleSidebar: () => void;
  language: Language;
};

function FolderIcon({ open = false }: { open?: boolean }) {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path
        d={
          open
            ? "M2.7 6.6h14.6l-1.45 8.05a1.4 1.4 0 0 1-1.38 1.15H4.42a1.4 1.4 0 0 1-1.38-1.15L2.7 6.6Z"
            : "M2.7 5.1c0-.77.63-1.4 1.4-1.4h3.02l1.45 1.55h7.33c.77 0 1.4.63 1.4 1.4v8.05c0 .77-.63 1.4-1.4 1.4H4.1c-.77 0-1.4-.63-1.4-1.4V5.1Z"
        }
      />
    </svg>
  );
}

function PlusIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M10 3.5v13M3.5 10h13" />
    </svg>
  );
}

function RunIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M5.2 3.2h7l3 3v10.6H5.2V3.2Z" />
      <path d="M12.2 3.2v3h3M8 10h4.7M8 13h3.2" />
    </svg>
  );
}

function TrashIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M4.6 6.2h10.8" />
      <path d="M8.2 6.2V4.4h3.6v1.8" />
      <path d="M6.1 6.2 6.8 16h6.4l.7-9.8" />
      <path d="M8.8 8.7v4.8M11.2 8.7v4.8" />
    </svg>
  );
}

function CopyInputsIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.4">
      <rect x="6.5" y="6.5" width="8" height="8" rx="1.4" />
      <path d="M13.5 6.5V5.2A1.2 1.2 0 0 0 12.3 4H6.2A1.2 1.2 0 0 0 5 5.2v6.1A1.2 1.2 0 0 0 6.2 12.6h1.3" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <circle cx="10" cy="10" r="2.6" />
      <path d="M10 2.8v1.5M10 15.7v1.5M2.8 10h1.5M15.7 10h1.5M4.9 4.9 6 6M14 14l1.1 1.1M15.1 4.9 14 6M6 14l-1.1 1.1" />
      <circle cx="10" cy="10" r="6.1" />
    </svg>
  );
}

function SidebarToggleIcon({ collapsed }: { collapsed: boolean }) {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <rect x="2.7" y="3.2" width="14.6" height="13.6" rx="1.4" />
      <path d="M7.1 3.2v13.6" />
      <path d={collapsed ? "m11.2 7.2 2.8 2.8-2.8 2.8" : "m13.8 7.2-2.8 2.8 2.8 2.8"} />
    </svg>
  );
}

function relativeTime(value: string, language: Language) {
  const t = (message: string, params?: Record<string, string | number>) =>
    translate(language, message, params);
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return "";
  const elapsed = Math.max(0, Date.now() - timestamp);
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return t("Just now");
  if (minutes < 60) return t("{count} min", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("{count} hr", { count: hours });
  const days = Math.floor(hours / 24);
  if (days < 30) return t("{count} days", { count: days });
  return new Intl.DateTimeFormat(language, {
    month: "numeric",
    day: "numeric",
  }).format(new Date(timestamp));
}

function runTone(statusBucket: string) {
  if (statusBucket === "completed") return "complete";
  if (statusBucket === "failed_or_cancelled") return "failed";
  if (statusBucket === "ready_to_apply") return "ready";
  return "active";
}

function runTitle(objective: string, language: Language) {
  return objective && objective !== "(无目标)"
    ? objective
    : translate(language, "Untitled task");
}

function workflowLabel(state: string, language: Language, legacyReadOnly = false) {
  if (legacyReadOnly) return translate(language, "Legacy · read-only");
  const labels: Record<string, string> = {
    Created: "Define intent",
    GeneratingTests: "Generating",
    FreezingRevision: "Freeze candidate",
    ValidatingRevision: "Validating",
    PendingTestReview: "Test review",
    ExecutionReady: "Ready to execute",
    ExecutingTests: "Executing",
    PendingResultReview: "Result review",
    ReadyToApply: "Ready to apply",
    ApplyingRevision: "Applying",
    NeedsHuman: "Needs attention",
    Completed: "Completed",
    Failed: "Failed",
    Cancelled: "Cancelled",
    Rejected: "Rejected",
    Superseded: "Superseded",
  };
  return translate(language, labels[state] ?? state);
}

export function WorkspaceSidebar({
  projects,
  runs,
  activeProjectId,
  activeRunId,
  busy,
  onSelectProject,
  onSelectRun,
  onCreateRun,
  onDeleteRun,
  onCopyRunInputs,
  onAddProject,
  onOpenSettings,
  serviceReady,
  sidebarCollapsed,
  onToggleSidebar,
  language,
}: WorkspaceSidebarProps) {
  const t = (message: string, params?: Record<string, string | number>) =>
    translate(language, message, params);
  const [collapsedProjects, setCollapsedProjects] = useState<Set<string>>(
    () => new Set(),
  );
  const runsByProject = useMemo(() => {
    const grouped = new Map<string, SidebarRun[]>();
    for (const run of runs) {
      const projectRuns = grouped.get(run.project_id) ?? [];
      projectRuns.push(run);
      grouped.set(run.project_id, projectRuns);
    }
    return grouped;
  }, [runs]);

  useEffect(() => {
    if (!activeProjectId) return;
    setCollapsedProjects((previous) => {
      if (!previous.has(activeProjectId)) return previous;
      const next = new Set(previous);
      next.delete(activeProjectId);
      return next;
    });
  }, [activeProjectId]);

  function toggleProject(projectId: string) {
    setCollapsedProjects((previous) => {
      const next = new Set(previous);
      if (next.has(projectId)) next.delete(projectId);
      else next.add(projectId);
      return next;
    });
  }

  return (
    <aside
      className={`workspace-sidebar ${sidebarCollapsed ? "is-collapsed" : ""}`}
      aria-label={t("Projects and tasks")}
    >
      <div className="sidebar-brand">
        {!sidebarCollapsed && (
          <>
            <span className="product-mark" aria-hidden="true">
              TH
            </span>
            <span className="sidebar-brand-copy">
              <strong>TestHarbor</strong>
              <small>LOCAL QA WORKBENCH</small>
            </span>
          </>
        )}
        <button
          type="button"
          className="sidebar-toggle"
          onClick={onToggleSidebar}
          aria-label={sidebarCollapsed ? t("Expand sidebar") : t("Collapse sidebar")}
          aria-expanded={!sidebarCollapsed}
          title={`${sidebarCollapsed ? t("Expand sidebar") : t("Collapse sidebar")} (⌘ B)`}
        >
          <SidebarToggleIcon collapsed={sidebarCollapsed} />
        </button>
      </div>

      <button
        type="button"
        className="sidebar-new-task"
        disabled={busy}
        title={sidebarCollapsed ? t("New test task") : undefined}
        onClick={() =>
          onCreateRun(
            projects.find((item) => item.id === activeProjectId) ?? projects[0] ?? null,
          )
        }
      >
        <PlusIcon />
        <span>{t("New test task")}</span>
        <kbd>⌘ N</kbd>
      </button>

      {sidebarCollapsed ? (
        <nav className="sidebar-compact-projects" aria-label={t("Project shortcuts")}>
          {projects.map((project) => (
            <button
              type="button"
              key={project.id}
              className={project.id === activeProjectId ? "active" : ""}
              onClick={() => onSelectProject(project)}
              title={project.name}
              aria-label={t("Open project {project}", { project: project.name })}
            >
              <FolderIcon open={project.id === activeProjectId} />
            </button>
          ))}
          <button
            type="button"
            disabled={busy}
            onClick={onAddProject}
            title={t("Add project")}
            aria-label={t("Add project")}
          >
            <PlusIcon />
          </button>
        </nav>
      ) : (
        <>
          <div className="sidebar-section-heading">
            <span>{t("Project")}</span>
            <button
              type="button"
              disabled={busy}
              onClick={onAddProject}
              aria-label={t("Add project")}
            >
              <PlusIcon />
            </button>
          </div>

          <div className="sidebar-projects">
            {projects.map((project) => {
              const allProjectRuns = runsByProject.get(project.id) ?? [];
              const projectRuns = allProjectRuns;
              const isProjectCollapsed = collapsedProjects.has(project.id);
              const isActive = project.id === activeProjectId;
              return (
                <section
                  className={`sidebar-project ${isActive ? "active-project" : ""}`}
                  key={project.id}
                >
                  <div className="sidebar-project-row">
                    <button
                      type="button"
                      className="sidebar-project-main"
                      onClick={() => onSelectProject(project)}
                      title={project.root_path}
                    >
                      <FolderIcon open={isActive} />
                      <span>
                        <strong>{project.name}</strong>
                        <small>{t("{count} tasks", { count: allProjectRuns.length })}</small>
                      </span>
                    </button>
                    <button
                      type="button"
                      className={`sidebar-chevron ${
                        isProjectCollapsed ? "collapsed" : ""
                      }`}
                      onClick={() => toggleProject(project.id)}
                      aria-label={
                        isProjectCollapsed
                          ? t("Expand {project}", { project: project.name })
                          : t("Collapse {project}", { project: project.name })
                      }
                      aria-expanded={!isProjectCollapsed}
                    >
                      <span>⌄</span>
                    </button>
                    <button
                      type="button"
                      className="sidebar-project-add"
                      disabled={busy}
                      onClick={() => onCreateRun(project)}
                      aria-label={t("Create a task in {project}", { project: project.name })}
                    >
                      <PlusIcon />
                    </button>
                  </div>

                  {!isProjectCollapsed && (
                    <div className="sidebar-runs">
                      {projectRuns.map((run) => (
                        <div
                          className={`sidebar-run-row ${
                            run.run_id === activeRunId ? "active" : ""
                          }`}
                          key={run.run_id}
                        >
                          <button
                            type="button"
                            className="sidebar-run-main"
                            onClick={() => onSelectRun(run)}
                            title={run.objective}
                          >
                            <span
                              className={`run-status-dot ${runTone(run.status_bucket)}`}
                            />
                            <RunIcon />
                            <span className="sidebar-run-copy">
                              <strong>{runTitle(run.objective, language)}</strong>
                              <small>{workflowLabel(run.workflow_state, language, run.legacy_read_only)}</small>
                            </span>
                            <time>{relativeTime(run.updated_at, language)}</time>
                          </button>
                          <button
                            type="button"
                            className="sidebar-run-copy-action"
                            disabled={busy}
                            onClick={(event) => {
                              event.stopPropagation();
                              onCopyRunInputs(run);
                            }}
                            title={t("Copy inputs to new task")}
                            aria-label={t("Copy inputs to new task")}
                          >
                            <CopyInputsIcon />
                          </button>
                          <button
                            type="button"
                            className="sidebar-run-delete"
                            disabled={busy}
                            onClick={() => onDeleteRun(run)}
                            title={t("Delete task {task}", { task: runTitle(run.objective, language) })}
                            aria-label={t("Delete task {task}", { task: runTitle(run.objective, language) })}
                          >
                            <TrashIcon />
                          </button>
                        </div>
                      ))}
                      {projectRuns.length === 0 && (
                        <button
                          type="button"
                          className="sidebar-empty-run"
                          disabled={busy}
                          onClick={() => onCreateRun(project)}
                        >
                          <PlusIcon />
                          {t("Create the first task in this project")}
                        </button>
                      )}
                    </div>
                  )}
                </section>
              );
            })}
            {projects.length === 0 && (
              <div className="sidebar-no-projects">
                <p>{t("No projects yet")}</p>
                <button type="button" disabled={busy} onClick={onAddProject}>
                  {t("Open local repository")}
                </button>
              </div>
            )}
          </div>
        </>
      )}

      <div className="sidebar-footer">
        <div className="sidebar-service-state">
          <span className={serviceReady ? "ready" : "offline"} />
          {!sidebarCollapsed && (
            <span>{serviceReady ? t("Generation service ready") : t("Generation service requires configuration")}</span>
          )}
        </div>
        <button
          type="button"
          onClick={onOpenSettings}
          title={sidebarCollapsed ? t("System settings") : undefined}
          aria-label={t("System settings")}
        >
          <SettingsIcon />
          {!sidebarCollapsed && (
            <>
              <span>{t("System settings")}</span>
              <span>⌘ ,</span>
            </>
          )}
        </button>
      </div>
    </aside>
  );
}
