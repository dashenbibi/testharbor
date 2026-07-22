import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { WorkspaceSidebar } from "./WorkspaceSidebar";
import { normalizeLanguage, setActiveLanguage, tr, translate, type Language } from "./i18n";
import {
  Background,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  type Edge,
  type Node,
  type NodeProps,
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import "./App.css";

type Tab = "project" | "run";

type AppInfo = {
  name: string;
  version: string;
  domain_ready: boolean;
  data_dir: string | null;
};

type CodexEnvironment = {
  ready: boolean;
  version: string | null;
  authenticated: boolean;
  auth_status: string | null;
  has_output_schema: boolean;
  has_ephemeral: boolean;
  has_sandbox: boolean;
  error: string | null;
};

type ClaudeEnvironment = {
  ready: boolean;
  version: string | null;
  authenticated: boolean;
  auth_status: string | null;
  has_print: boolean;
  has_stream_json: boolean;
  has_json_schema: boolean;
  has_resume: boolean;
  error: string | null;
};

type GrokEnvironment = {
  ready: boolean;
  version: string | null;
  authenticated: boolean;
  auth_status: string | null;
  has_headless: boolean;
  has_json: boolean;
  has_resume: boolean;
  error: string | null;
};

type SelectOption = {
  value: string;
  label: string;
  enabled: boolean;
};

type CliModelCatalog = {
  models: string[];
  default_model: string | null;
};

type AgentPreferences = {
  enabled_modes: string[];
};

type UiPreferences = {
  language: Language;
};

const cliInstallUrls: Record<string, string> = {
  codex: "https://help.openai.com/en/articles/11096431",
  claude: "https://docs.anthropic.com/en/docs/claude-code/getting-started",
  grok: "https://x.ai/cli",
};

function cliInstalled(environment: { version: string | null; error: string | null } | null) {
  if (!environment) return false;
  if (environment.version) return true;
  const error = (environment.error ?? "").toLowerCase();
  return ![
    "not found",
    "no such file",
    "os error 2",
    "未找到",
    "未检测到",
  ].some((marker) => error.includes(marker));
}

function selectConfiguredAgentMode(
  preferred: string,
  preferences: AgentPreferences,
  environments: {
    codex: { ready: boolean };
    claude: { ready: boolean };
    grok: { ready: boolean };
  },
) {
  const readyModes = [
    { mode: "codex", ready: environments.codex.ready },
    { mode: "claude", ready: environments.claude.ready },
    { mode: "grok", ready: environments.grok.ready },
  ]
    .filter(({ mode, ready }) => preferences.enabled_modes.includes(mode) && ready)
    .map(({ mode }) => mode);
  return readyModes.includes(preferred) ? preferred : readyModes[0] ?? "";
}

function HelpHint({ message }: { message: ReactNode }) {
  return (
    <span
      className="help-hint"
      tabIndex={0}
      role="note"
      aria-label={tr("View help")}
    >
      <span className="help-hint-mark" aria-hidden="true">?</span>
      <span className="help-hint-popover" role="tooltip">{message}</span>
    </span>
  );
}

function AgentSelect({
  value,
  grokReady,
  claudeReady,
  options: suppliedOptions,
  disabled,
  onChange,
}: {
  value: string;
  grokReady?: boolean;
  claudeReady?: boolean;
  options?: SelectOption[];
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  const options = suppliedOptions ?? [
    { value: "codex", label: "Codex CLI", enabled: true },
    { value: "claude", label: "Claude Code CLI", enabled: Boolean(claudeReady) },
    { value: "grok", label: "Grok CLI", enabled: Boolean(grokReady) },
  ];
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const current =
    options.find((option) => option.value === value) ??
    options[0] ??
    { value: "", label: "未配置生成服务", enabled: false };

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as globalThis.Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const choose = (nextValue: string) => {
    const next = options.find((option) => option.value === nextValue);
    if (!next?.enabled) return;
    onChange(next.value);
    setOpen(false);
  };

  const move = (direction: 1 | -1) => {
    const available = options.filter((option) => option.enabled);
    if (available.length === 0) return;
    const currentIndex = Math.max(
      0,
      available.findIndex((option) => option.value === current.value),
    );
    const next = available[(currentIndex + direction + available.length) % available.length];
    choose(next.value);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "Escape") {
      setOpen(false);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowRight") {
      event.preventDefault();
      if (!open) setOpen(true);
      else move(1);
    } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
      event.preventDefault();
      if (!open) setOpen(true);
      else move(-1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      setOpen((currentOpen) => !currentOpen);
    }
  };

  return (
    <div className={`agent-select${open ? " open" : ""}`} ref={ref}>
      <button
        type="button"
        className="agent-select-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((currentOpen) => !currentOpen)}
        onKeyDown={handleKeyDown}
      >
        <span>{current.label}</span>
        <span className="agent-select-chevron" aria-hidden="true">⌄</span>
      </button>
      {open && (
        <div className="agent-select-menu" role="listbox" aria-label={tr("Generation service")}>
          {options.map((option) => (
            <button
              key={option.value}
              type="button"
              role="option"
              disabled={!option.enabled}
              aria-selected={value === option.value}
              className={value === option.value ? "active" : ""}
              onClick={() => choose(option.value)}
              onKeyDown={handleKeyDown}
            >
              <span>{option.label}</span>
              {!option.enabled ? (
                <small>{tr("Not ready")}</small>
              ) : value === option.value ? (
                <span aria-hidden="true">✓</span>
              ) : null}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

type RecoveryReport = {
  messages: string[];
  marked_lost: string[];
  orphan_worktrees_removed: string[];
};

function recoveryNotice(report: RecoveryReport | null): string | null {
  if (!report) return null;
  if (report.marked_lost.length > 0) {
    return `启动恢复：${report.marked_lost.length} 条 Run 因进程丢失标为 NeedsHuman。可到历史继续。`;
  }
  if (report.orphan_worktrees_removed.length > 0) {
    return `启动恢复：已清理 ${report.orphan_worktrees_removed.length} 个孤儿 worktree。`;
  }
  return null;
}

type ProjectSummary = {
  id: string;
  root_path: string;
  name: string;
  git_remote: string | null;
  last_opened_at?: string;
};

type GitRepoInfoDto = {
  root: string;
  head_commit: string;
  short_commit: string;
  branch: string | null;
  is_dirty: boolean;
  apply_hint: string;
};

type ReadinessCheck = {
  key: string;
  label: string;
  status: "passed" | "not_run" | "failed" | "inconclusive";
  detail: string;
};

type ProjectProfileSummary = {
  key: string;
  label: string;
  command_preview: string;
  parser: string;
  structured: boolean;
  strict: boolean;
  available: boolean;
  recommended: boolean;
};

type ProjectReadiness = {
  detected_stacks: string[];
  test_file_count: number;
  capability: "strict_accept_pass_apply" | "run_only" | "config_incomplete";
  capability_label: string;
  summary: string;
  profiles: ProjectProfileSummary[];
  checks: ReadinessCheck[];
};

type OpenProjectResult = {
  project: ProjectSummary;
  repo: GitRepoInfoDto;
  readiness: ProjectReadiness;
  created_run: {
    run_id: string;
    workflow_state: string;
    base_commit: string;
  } | null;
};

type CreatedRunSummary = NonNullable<OpenProjectResult["created_run"]>;

type RunHistoryItem = {
  run_id: string;
  project_id: string;
  project_name: string;
  project_root: string;
  objective: string;
  acceptance_criteria: string[];
  workflow_state: string;
  delivery_status: string;
  result_verdict: string | null;
  base_commit_short: string;
  has_worktree: boolean;
  updated_at: string;
  created_at?: string;
  status_bucket: string;
  resume_hint: string;
  can_retry_generation: boolean;
  legacy_read_only: boolean;
  agent_mode: string | null;
  agent_model: string | null;
};

type ProjectCaseRecord = {
  key: string;
  case_id: string;
  label: string;
  objective: string;
  step_count: number;
  approved: boolean;
  can_rerun: boolean;
  run: RunHistoryItem;
};

type DiskUsage = {
  data_dir: string;
  total_bytes: number;
  worktrees_bytes: number;
  artifacts_bytes: number;
  runs_bytes: number;
  db_bytes: number;
};

type Activity = {
  kind: "project" | "generate" | "execute" | "review" | "delivery" | "maintenance";
  title: string;
  detail: string;
  run_id?: string;
};

type ProcessOutcomeDto = {
  phase: "starting" | "running" | "exited" | "timed_out" | "cancelled" | "failed";
  exit_code: number | null;
  started_at: string;
  finished_at: string | null;
};

type ExecutionPollDto = {
  outcome: ProcessOutcomeDto;
  stdout_tail: string;
  stderr_tail: string;
  generic_result: { summary: string } | null;
  accept_pass: { enabled: boolean; reasons: string[] } | null;
  structured: { total: number; passed: number; failed: number } | null;
  analysis: {
    suggested_verdict: string;
    confidence: string;
    summary: string;
    findings: { severity: string; message: string }[];
  } | null;
  result_review: {
    recommendation: string;
    analyzer_overreach: boolean;
    summary: string;
    findings: { severity: string; message: string }[];
  } | null;
  agent_error: string | null;
};

function fmtBytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function readinessTone(
  status: ReadinessCheck["status"],
): "success" | "warning" | "danger" | "neutral" {
  switch (status) {
    case "passed":
      return "success";
    case "failed":
      return "danger";
    case "not_run":
    case "inconclusive":
      return "warning";
  }
}

function readinessStatusLabel(status: ReadinessCheck["status"]) {
  switch (status) {
    case "passed":
      return tr("Passed");
    case "failed":
      return tr("Failed");
    case "not_run":
      return tr("Not run");
    case "inconclusive":
      return tr("Pending confirmation");
  }
}

function capabilityTone(
  capability: ProjectReadiness["capability"] | null,
): "success" | "warning" | "danger" | "neutral" {
  switch (capability) {
    case "strict_accept_pass_apply":
      return "success";
    case "run_only":
      return "warning";
    case "config_incomplete":
      return "danger";
    default:
      return "neutral";
  }
}

function phaseLabel(phase: ProcessOutcomeDto["phase"]) {
  switch (phase) {
    case "starting":
      return tr("Starting");
    case "running":
      return tr("Running");
    case "exited":
      return tr("Execution complete");
    case "timed_out":
      return tr("Execution timed out");
    case "cancelled":
      return tr("Cancelled");
    case "failed":
      return tr("Startup failed");
  }
}

function formatProcessDuration(outcome: ProcessOutcomeDto) {
  if (!outcome.finished_at) return tr("In progress");
  const started = new Date(outcome.started_at).getTime();
  const finished = new Date(outcome.finished_at).getTime();
  if (!Number.isFinite(started) || !Number.isFinite(finished) || finished < started) {
    return "—";
  }
  const seconds = (finished - started) / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${Math.round(seconds % 60)}s`;
}

function operationErrorMessage(error: unknown) {
  const raw = String(error);
  const lower = raw.toLowerCase();
  let summary: string;
  if (
    lower.includes("no conversation found") ||
    (lower.includes("claude") && lower.includes("session id"))
  ) {
    summary = tr("The Claude Code session expired. TestHarbor will retry with a new session; if it still fails, run `claude auth status` in a terminal.");
  } else if (lower.includes("claude.ai connectors are disabled")) {
    summary = tr("Claude Code detected an authentication source such as ANTHROPIC_API_KEY taking precedence over claude.ai login. Check the current terminal environment.");
  } else if (
    lower.includes("not found") ||
    lower.includes("no such file") ||
    lower.includes("os error 2")
  ) {
    summary = tr("Codex was not detected. Install it and refresh service status.");
  } else if (
    lower.includes("grok 模型服务") ||
    lower.includes("cli-chat-proxy") ||
    lower.includes("network")
  ) {
    summary = tr("The Grok model service could not connect. Check HTTP(S)/SOCKS proxy settings and restart TestHarbor.");
  } else if (
    lower.includes("login") ||
    lower.includes("auth") ||
    lower.includes("401") ||
    lower.includes("unauthorized")
  ) {
    summary = tr("Codex authentication expired. Run `codex login` in a terminal, then refresh service status.");
  } else if (lower.includes("output-schema") || lower.includes("ephemeral")) {
    summary = tr("This Codex version is unsupported. Upgrade it and refresh service status.");
  } else if (lower.includes("no stdout/stderr activity")) {
    summary = tr("The generation service stopped producing output. Check network and service status, then retry.");
  } else if (lower.includes("cancelled by user") || lower.includes("generation cancelled")) {
    summary = tr("Generation was terminated. The isolated directory is retained so you can continue or regenerate.");
  } else if (
    lower.includes("candidatetreemismatch") ||
    lower.includes("candidate_tree_mismatch") ||
    lower.includes("start execution blocked") && lower.includes("candidate")
  ) {
    summary = tr("The approved isolated worktree changed, so TestHarbor blocked execution. Inspect the changed files; remove temporary files and retry, or regenerate/reapprove changed test source.");
  } else if (
    lower.includes("start execution blocked") &&
    (lower.includes("wrongworkflowstate") ||
      lower.includes("needs_human") ||
      lower.includes("needshuman"))
  ) {
    summary = tr("This task requires human review. Resolve or accept the review findings, approve the case again, and then start formal execution.");
  } else if (lower.includes("changes outside writable scope")) {
    summary = tr("The generation service changed files outside the test scope, so TestHarbor blocked the candidate. Restrict output to test files or configure the required test paths/names.");
  } else if (
    lower.includes("path policy violation") ||
    lower.includes("其他 cli") ||
    lower.includes("外部修改")
  ) {
    summary = tr("Another CLI or external change was detected in the TestHarbor isolated directory. TestHarbor stopped to protect those files; finish and save the other changes, then retry.");
  } else if (
    lower.includes("runtime limit") ||
    lower.includes("timed out") ||
    lower.includes("timeout")
  ) {
    summary = tr("Generation reached its runtime limit. Narrow the test objective and retry.");
  } else {
    summary = tr("The operation failed. Check the technical details and runtime environment.");
  }
  return `${summary}\n\n${tr("Technical details")}\n${raw}`;
}

type ReviewFindingDto = {
  id: string;
  severity: string;
  category: string;
  file: string | null;
  line: number | null;
  message: string;
  suggestion: string | null;
  disposition: string;
  risk_reason: string | null;
};

type FileDiffSlice = {
  path: string;
  patch: string;
  additions: number;
  deletions: number;
};

type CaseReviewDetail = {
  run_id: string;
  workflow_state: string;
  objective: string;
  acceptance_criteria: string[];
  review: {
    recommendation: string;
    summary: string;
    findings: ReviewFindingDto[];
    disposition_snapshot_hash: string;
  };
  candidate: {
    sequence: number;
    base_commit: string;
    candidate_commit: string;
    candidate_tree: string;
    changed_paths: string[];
  } | null;
  patch: string;
  file_diffs: FileDiffSlice[];
  approved: boolean;
  can_approve: boolean;
  approve_blockers: string[];
  intended_count: number;
  pending_change_request: string | null;
  can_retry_generation: boolean;
  agent_mode: string | null;
  agent_model: string | null;
  reuse_agent_session: boolean;
  environment: {
    context: string;
    base_url: string;
    health_url: string | null;
    login_url: string | null;
    startup_command: string | null;
    startup_cwd: string | null;
    credentials_provided: boolean;
    variable_names: string[];
  } | null;
};

type TestEnvironmentInput = {
  context: string;
  base_url: string;
  health_url: string;
  login_url: string;
  startup_command: string;
  startup_cwd: string;
  username: string;
  password: string;
  variables: Record<string, string>;
};

const DEFAULT_TEST_OBJECTIVE = "编排接口 E2E 流程测试，验证完整业务链路";
const DEFAULT_ACCEPTANCE_CRITERIA =
  "通过 API/接口完成主要操作流程\n查询数据库或缓存验证关键状态变化\n测试数据隔离且按设置清理";

function emptyTestEnvironment(): TestEnvironmentInput {
  return {
    context: "",
    base_url: "",
    health_url: "",
    login_url: "",
    startup_command: "",
    startup_cwd: "",
    username: "",
    password: "",
    variables: {},
  };
}

function normDisp(d: string) {
  const x = d.toLowerCase();
  if (x === "mustfix" || x === "must_fix") return "must_fix";
  if (x === "acceptedrisk" || x === "accepted_risk") return "accepted_risk";
  if (x === "resolved") return "resolved";
  return "open";
}

function reviewRecommendationLabel(value: string) {
  switch (value.trim().toUpperCase()) {
    case "APPROVE":
      return tr("Approve recommended");
    case "CHANGES":
    case "CHANGE_TEST":
      return tr("Changes required");
    case "BLOCK":
      return tr("Blocked");
    case "NEEDS_HUMAN":
      return tr("Human review required");
    case "ACCEPT_PASS":
      return tr("Accept pass");
    case "ACCEPT_FAIL":
      return tr("Accept fail");
    case "RERUN":
      return tr("Rerun");
    default:
      return value || tr("Not provided");
  }
}

function reviewSeverityLabel(value: string) {
  switch (value.trim().toUpperCase()) {
    case "CRITICAL":
      return tr("Critical");
    case "HIGH":
      return tr("High risk");
    case "MEDIUM":
      return tr("Medium risk");
    case "LOW":
      return tr("Low risk");
    case "INFO":
      return tr("Info");
    default:
      return value || tr("Unspecified");
  }
}

function reviewSeverityRank(value: string) {
  switch (value.trim().toUpperCase()) {
    case "CRITICAL":
    case "BLOCKER":
    case "FATAL":
      return 0;
    case "HIGH":
    case "ERROR":
    case "MUST_FIX":
    case "MUST-FIX":
      return 1;
    case "MEDIUM":
    case "WARNING":
    case "WARN":
      return 2;
    case "LOW":
      return 3;
    case "INFO":
    case "INFORMATIONAL":
      return 4;
    default:
      return 5;
  }
}

function reviewDispositionLabel(value: string) {
  switch (normDisp(value)) {
    case "must_fix":
      return tr("Must fix");
    case "resolved":
      return tr("Resolved");
    case "accepted_risk":
      return tr("Accepted risk");
    default:
      return tr("Pending");
  }
}

function resultActionLabel(value: string) {
  switch (value.toLowerCase()) {
    case "run":
      return tr("Running");
    case "pass":
      return tr("Passed");
    case "fail":
      return tr("Failed");
    case "skip":
      return tr("Skipped");
    default:
      return value;
  }
}

const dispositionOptions = [
  { value: "open", label: "Pending" },
  { value: "must_fix", label: "Must fix" },
  { value: "resolved", label: "Resolved" },
  { value: "accepted_risk", label: "Accepted risk" },
] as const;

function ReviewDispositionSelect({
  value,
  disabled,
  onChange,
}: {
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!ref.current?.contains(event.target as globalThis.Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const label = reviewDispositionLabel(value);
  return (
    <div className={`review-select${open ? " open" : ""}`} ref={ref}>
      <button
        type="button"
        className="review-select-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <span>{label}</span>
        <span className="review-select-chevron" aria-hidden="true">⌄</span>
      </button>
      {open && (
        <div className="review-select-menu" role="listbox" aria-label={tr("Disposition options")}>
          {dispositionOptions.map((option) => (
            <button
              key={option.value}
              type="button"
              role="option"
              aria-selected={value === option.value}
              className={value === option.value ? "active" : ""}
              onClick={() => {
                onChange(option.value);
                setOpen(false);
              }}
            >
              <span>{tr(option.label)}</span>
              {value === option.value && <span aria-hidden="true">✓</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function StateMark({
  tone = "neutral",
  label,
}: {
  tone?: "success" | "warning" | "danger" | "neutral" | "active";
  label: string;
}) {
  return (
    <span className={`state-mark tone-${tone}`}>
      <span className="state-dot" aria-hidden="true" />
      {label}
    </span>
  );
}

type ParsedTestRow = {
  name: string;
  pkg: string;
  action: string;
  elapsed: number | null;
};

type GenerationProgressDto = {
  stdout_tail: string;
  stderr_tail: string;
};

function parseTestRows(output: string): ParsedTestRow[] {
  const rows = new Map<string, ParsedTestRow>();
  for (const line of output.split("\n")) {
    if (!line.trim().startsWith("{")) continue;
    try {
      const item = JSON.parse(line) as {
        Action?: string;
        Test?: string;
        Package?: string;
        Elapsed?: number;
      };
      if (!item.Test || !item.Action) continue;
      if (!["run", "pass", "fail", "skip"].includes(item.Action)) continue;
      rows.set(`${item.Package ?? ""}:${item.Test}`, {
        name: item.Test,
        pkg: item.Package ?? "当前包",
        action: item.Action,
        elapsed: typeof item.Elapsed === "number" ? item.Elapsed : null,
      });
    } catch {
      // 普通测试输出不是 JSON；保留给原始证据终端显示。
    }
  }
  return Array.from(rows.values()).slice(-40);
}

function resultTone(
  action: string,
): "success" | "warning" | "danger" | "neutral" | "active" {
  if (action === "pass") return "success";
  if (action === "fail") return "danger";
  if (action === "skip") return "warning";
  return "active";
}

type CaseFlowStep = {
  key: string;
  caseId?: string;
  label: string;
  input: string;
  action: string;
  output: string;
  assertion: string;
  request?: CaseFlowHttpEvidence;
  response?: CaseFlowHttpEvidence;
  kind:
    | "setup"
    | "auth"
    | "api"
    | "mysql"
    | "redis"
    | "database"
    | "cache"
    | "queue"
    | "filesystem"
    | "assert"
    | "cleanup"
    | "business";
};

type CaseFlowHttpEvidence = {
  method?: string;
  url?: string;
  status?: string | number;
  headers?: unknown;
  body?: unknown;
};

type CaseFlowCase = {
  id: string;
  label: string;
  objective: string;
  steps: CaseFlowStep[];
};

function containsAny(text: string, words: string[]) {
  const lower = text.toLowerCase();
  return words.some((word) => lower.includes(word.toLowerCase()));
}

function buildCaseFlowCases(
  detail: CaseReviewDetail,
  execution: ExecutionPollDto | null,
): CaseFlowCase[] {
  const patchText = detail.file_diffs.map((file) => file.patch).join("\n");
  const runtimeText = execution
    ? `${execution.stdout_tail}\n${execution.stderr_tail}`
    : "";
  const text = `${patchText}\n${runtimeText}`;
  const steps = parseTestHarborSteps(text);
  const cases = new Map<string, CaseFlowCase>();

  for (const rawLine of text.split("\n")) {
    let line = rawLine;
    if (rawLine.trim().startsWith("{")) {
      try {
        const event = JSON.parse(rawLine) as { Output?: string };
        if (typeof event.Output === "string") line = event.Output;
      } catch {
        // Parse plain text below when this is not a go test JSON event.
      }
    }
    const item = parseMarkerJson(line, "TESTHARBOR_CASE");
    if (!item) continue;
    const id = String(item.id ?? item.case_id ?? item.caseId ?? "");
    if (!id) continue;
    cases.set(id, {
      id,
      label: String(item.title ?? item.label ?? id),
      objective: String(item.objective ?? item.description ?? ""),
      steps: cases.get(id)?.steps ?? [],
    });
  }

  for (const step of steps) {
    const id = step.caseId ?? "default";
    const current = cases.get(id) ?? {
      id,
      label: id === "default" ? "完整业务流程" : id,
      objective: "",
      steps: [],
    };
    current.steps.push(step);
    cases.set(id, current);
  }

  return Array.from(cases.values()).filter((item) => item.steps.length > 0);
}

function classifyCaseStep(id: string, title: string): CaseFlowStep["kind"] {
  const text = `${id} ${title}`.toLowerCase();
  if (containsAny(text, ["cleanup", "clean", "delete", "清理"])) return "cleanup";
  if (containsAny(text, ["assert", "expect", "断言", "校验"])) return "assert";
  if (containsAny(text, ["login", "sign in", "signin", "auth", "token", "cookie", "登录", "鉴权", "认证"])) return "auth";
  if (containsAny(text, ["mysql", "mariadb", "sql", "db", "database", "数据库"])) return "mysql";
  if (containsAny(text, ["redis", "缓存", "cache", "key"])) return "redis";
  if (containsAny(text, ["queue", "kafka", "消息", "队列"])) return "queue";
  if (containsAny(text, ["file", "filesystem", "文件", "对象存储"])) return "filesystem";
  if (containsAny(text, ["state", "状态", "storage", "存储"])) return "database";
  if (containsAny(text, ["api", "http", "request", "接口", "调用"])) return "api";
  if (containsAny(text, ["setup", "准备", "fixture", "数据准备"])) return "setup";
  if (containsAny(text, ["create", "update", "publish", "configure", "创建", "配置", "发布"])) {
    return "business";
  }
  return "business";
}

function isCaseStepKind(value: string): value is CaseFlowStep["kind"] {
  return [
    "setup", "auth", "api", "mysql", "redis", "database", "cache", "queue",
    "filesystem", "assert", "cleanup", "business",
  ].includes(value as CaseFlowStep["kind"]);
}

function caseStepKindLabel(kind: CaseFlowStep["kind"]) {
  return tr({
    setup: "Setup", auth: "Authentication", api: "API", mysql: "MySQL", redis: "Redis", database: "Database",
    cache: "Cache", queue: "Message queue", filesystem: "File / object storage", assert: "Assertion",
    cleanup: "Cleanup", business: "Business",
  }[kind]);
}

function splitGoCallArguments(body: string) {
  const args: string[] = [];
  let start = 0;
  let depth = 0;
  let quote = "";
  let escaped = false;
  for (let index = 0; index < body.length; index += 1) {
    const char = body[index];
    if (quote) {
      if (quote === "`" && char === "`") quote = "";
      else if (quote === '"' && !escaped && char === '"') quote = "";
      escaped = quote === '"' && !escaped && char === "\\";
      if (char !== "\\") escaped = false;
      continue;
    }
    if (char === '"' || char === "`") {
      quote = char;
      continue;
    }
    if (char === "(" || char === "[" || char === "{") depth += 1;
    else if (char === ")" || char === "]" || char === "}") depth -= 1;
    else if (char === "," && depth === 0) {
      args.push(body.slice(start, index).trim());
      start = index + 1;
    }
  }
  args.push(body.slice(start).trim());
  return args;
}

function describeGoStepArgument(value: string) {
  const trimmed = value.trim();
  if (trimmed.startsWith("`") && trimmed.endsWith("`")) {
    return trimmed.slice(1, -1);
  }
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) {
    try {
      return String(JSON.parse(trimmed));
    } catch {
      return trimmed.slice(1, -1);
    }
  }
  return trimmed;
}

function parseMarkerJson(line: string, marker: string) {
  const markerIndex = line.indexOf(marker);
  if (markerIndex < 0) return null;
  const jsonStart = line.indexOf("{", markerIndex);
  const jsonEnd = line.lastIndexOf("}");
  if (jsonStart < 0 || jsonEnd <= jsonStart) return null;
  const raw = line
    .slice(jsonStart, jsonEnd + 1)
    .replace(/\\"/g, '"')
    .replace(/\\\\/g, "\\");
  try {
    return JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function parseHttpEvidence(
  line: string,
  marker: "TESTHARBOR_HTTP_REQUEST" | "TESTHARBOR_HTTP_RESPONSE",
) {
  const item = parseMarkerJson(line, marker);
  if (!item) return null;
  const rawStepId = String(item.step_id ?? item.stepId ?? item.id ?? "");
  const caseId = String(item.case_id ?? item.caseId ?? "");
  if (!rawStepId) return null;
  const body = Object.prototype.hasOwnProperty.call(item, "body")
    ? item.body
    : item.response_body ?? item.responseBody;
  const headers = Object.prototype.hasOwnProperty.call(item, "headers")
    ? item.headers
    : item.response_headers ?? item.responseHeaders;
  return {
    stepId: caseId ? `${caseId}:${rawStepId}` : rawStepId,
    evidence: {
      method: typeof item.method === "string" ? item.method : undefined,
      url: typeof item.url === "string" ? item.url : undefined,
      status:
        typeof item.status === "string" || typeof item.status === "number"
          ? item.status
          : undefined,
      headers,
      body,
    } satisfies CaseFlowHttpEvidence,
  };
}

function formatCaseFlowEvidence(value: unknown) {
  if (value === undefined) return tr("Not collected");
  if (value === null) return "null";
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value, null, 2);
}

function formatHttpEvidence(evidence: CaseFlowHttpEvidence, response: boolean) {
  const lines: string[] = [];
  if (response && evidence.status != null) lines.push(`HTTP ${evidence.status}`);
  if (!response && evidence.method) {
    lines.push(`${evidence.method}${evidence.url ? ` ${evidence.url}` : ""}`);
  } else if (evidence.url) {
    lines.push(evidence.url);
  }
  if (evidence.headers !== undefined) {
    lines.push(`Headers\n${formatCaseFlowEvidence(evidence.headers)}`);
  }
  if (evidence.body !== undefined) {
    lines.push(`Body\n${formatCaseFlowEvidence(evidence.body)}`);
  } else if (response) {
    lines.push("Body\n未采集返回体");
  }
  return lines.join("\n");
}

function parseGoStepFunctionCalls(text: string): CaseFlowStep[] {
  const steps: CaseFlowStep[] = [];
  const callPattern = /\b[A-Za-z_][A-Za-z0-9_]*Step\s*\(/g;
  let match: RegExpExecArray | null;
  while ((match = callPattern.exec(text)) !== null) {
    const openIndex = text.indexOf("(", match.index);
    let closeIndex = openIndex;
    let depth = 0;
    let quote = "";
    let escaped = false;
    for (; closeIndex < text.length; closeIndex += 1) {
      const char = text[closeIndex];
      if (quote) {
        if (quote === "`" && char === "`") quote = "";
        else if (quote === '"' && !escaped && char === '"') quote = "";
        escaped = quote === '"' && !escaped && char === "\\";
        if (char !== "\\") escaped = false;
        continue;
      }
      if (char === '"' || char === "`") {
        quote = char;
      } else if (char === "(") {
        depth += 1;
      } else if (char === ")") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    if (depth !== 0) continue;
    const args = splitGoCallArguments(text.slice(openIndex + 1, closeIndex));
    if (args.length < 7) continue;
    const id = describeGoStepArgument(args[1]);
    const title = describeGoStepArgument(args[2]);
    if (!id || !title || id === "id" || title === "title") continue;
    steps.push({
      key: id,
      label: title,
      input: describeGoStepArgument(args[3]),
      action: describeGoStepArgument(args[4]),
      output: describeGoStepArgument(args[5]),
      assertion: describeGoStepArgument(args[6]),
      kind: classifyCaseStep(id, title),
    });
  }
  return steps;
}

function parseTestHarborSteps(text: string): CaseFlowStep[] {
  const steps = new Map<string, CaseFlowStep>();
  const httpEvidence = new Map<
    string,
    { request?: CaseFlowHttpEvidence; response?: CaseFlowHttpEvidence }
  >();
  for (const rawLine of text.split("\n")) {
    let line = rawLine;
    if (rawLine.trim().startsWith("{")) {
      try {
        const event = JSON.parse(rawLine) as { Output?: string };
        if (typeof event.Output === "string") line = event.Output;
      } catch {
        // Not a go test JSON event; parse as plain text below.
      }
    }
    for (const marker of ["TESTHARBOR_HTTP_REQUEST", "TESTHARBOR_HTTP_RESPONSE"] as const) {
      const parsed = parseHttpEvidence(line, marker);
      if (!parsed) continue;
      const current = httpEvidence.get(parsed.stepId) ?? {};
      if (marker === "TESTHARBOR_HTTP_REQUEST") current.request = parsed.evidence;
      else current.response = parsed.evidence;
      httpEvidence.set(parsed.stepId, current);
    }
    const item = parseMarkerJson(line, "TESTHARBOR_STEP");
    if (!item) continue;
    try {
      const rawId = String(item.id ?? item.key ?? `step-${steps.size + 1}`);
      const caseId = String(item.case_id ?? item.caseId ?? "");
      const id = caseId ? `${caseId}:${rawId}` : rawId;
      const title = String(item.title ?? item.label ?? `步骤 ${steps.size + 1}`);
      steps.set(id, {
        key: id,
        caseId: caseId || undefined,
        label: title,
        input: String(item.input ?? "未声明输入"),
        action: String(item.action ?? item.detail ?? "未声明动作"),
        output: String(item.output ?? "未声明输出"),
        assertion: String(item.assertion ?? item.assert ?? "未声明断言"),
        kind: isCaseStepKind(String(item.type ?? item.kind ?? ""))
          ? (String(item.type ?? item.kind) as CaseFlowStep["kind"])
          : classifyCaseStep(id, title),
      });
    } catch {
      // Ignore malformed marker and fall back to inferred steps.
    }
  }
  for (const step of parseGoStepFunctionCalls(text)) {
    if (!steps.has(step.key)) steps.set(step.key, step);
  }
  for (const [stepId, evidence] of httpEvidence) {
    const step = steps.get(stepId);
    if (step) steps.set(stepId, { ...step, ...evidence });
  }
  return Array.from(steps.values()).sort((left, right) => {
    const leftOrder = Number(left.key.match(/^\d+/)?.[0]);
    const rightOrder = Number(right.key.match(/^\d+/)?.[0]);
    const leftHasOrder = Number.isFinite(leftOrder);
    const rightHasOrder = Number.isFinite(rightOrder);
    if (!leftHasOrder || !rightHasOrder) return 0;
    return leftOrder - rightOrder;
  });
}

function parseObservedStepIds(output: string) {
  return parseTestHarborSteps(output).map((step) => step.key);
}

function caseStepExpectedHttpStatusSatisfied(step: CaseFlowStep | undefined) {
  if (!step?.response || step.response.status == null) return false;
  const actual = Number(String(step.response.status).match(/[1-5]\d\d/)?.[0]);
  if (!Number.isFinite(actual)) return false;
  const expectationText = `${step.output}\n${step.assertion}`.toLowerCase();
  const expectedStatuses = Array.from(expectationText.matchAll(/\b[1-5]\d\d\b/g))
    .map((match) => Number(match[0]));
  if (expectedStatuses.includes(actual)) return true;
  return (
    (actual === 401 || actual === 403) &&
    containsAny(expectationText, [
      "未认证", "未登录", "无认证", "无权限", "拒绝", "401", "403",
      "unauthorized", "forbidden", "not login", "permission denied",
    ])
  );
}

function caseStepObservedFailure(step: CaseFlowStep | undefined) {
  if (!step?.response) return false;
  const expectationText = `${step.output}\n${step.assertion}`.toLowerCase();
  const expectsSuccess = containsAny(expectationText, [
    "成功", "通过", "完成", "published", "success", "succeed",
  ]);
  if (!expectsSuccess) return false;
  const body = step.response.body;
  if (body && typeof body === "object") {
    const record = body as Record<string, unknown>;
    if (String(record.success ?? "").toLowerCase() === "fail") return true;
    const code = Number(record.code);
    if (Number.isFinite(code) && code < 0) return true;
  }
  return false;
}

function caseStepStatus({
  step,
  stepKey,
  index,
  total,
  execution,
  executionComplete,
  executionFailed,
}: {
  step?: CaseFlowStep;
  stepKey: string;
  index: number;
  total: number;
  execution: ExecutionPollDto | null;
  executionComplete: boolean;
  executionFailed: boolean;
}): CaseFlowStatus {
  if (!execution) return "planned";
  const observed = parseObservedStepIds(`${execution.stdout_tail}\n${execution.stderr_tail}`);
  if (observed.length > 0) {
    const currentObserved = observed[observed.length - 1];
    if (executionComplete) {
      if (observed.includes(stepKey)) {
        if (executionFailed && caseStepObservedFailure(step)) return "blocked";
        if (executionFailed && currentObserved === stepKey) {
          return caseStepExpectedHttpStatusSatisfied(step) ? "complete" : "blocked";
        }
        return "complete";
      }
      return executionFailed ? "skipped" : "planned";
    }
    if (currentObserved === stepKey) return "active";
    if (observed.includes(stepKey)) return "complete";
    return "planned";
  }
  if (executionComplete) {
    if (executionFailed && index >= Math.max(0, total - 2)) return "blocked";
    return "complete";
  }
  if (execution.outcome.phase === "starting") return index === 0 ? "active" : "planned";
  if (execution.outcome.phase === "running") {
    return index === 0 ? "active" : "planned";
  }
  return "planned";
}

type CaseFlowStatus = "planned" | "active" | "complete" | "blocked" | "skipped";

function caseFlowCaseStatus(
  item: CaseFlowCase,
  execution: ExecutionPollDto | null,
  executionComplete: boolean,
  executionFailed: boolean,
): CaseFlowStatus {
  if (!execution) return "planned";
  const statuses = item.steps.map((step, index) =>
    caseStepStatus({
      step,
      stepKey: step.key,
      index,
      total: item.steps.length,
      execution,
      executionComplete,
      executionFailed,
    }),
  );
  if (statuses.includes("blocked")) return "blocked";
  if (executionComplete && statuses.every((status) => status === "complete")) {
    return "complete";
  }
  if (executionComplete && executionFailed && statuses.includes("complete")) {
    return "blocked";
  }
  if (statuses.includes("active")) return "active";
  if (statuses.includes("complete")) return "active";
  return "planned";
}

type CaseFlowNodeData = {
  step: CaseFlowStep;
  index: number;
  status: CaseFlowStatus;
  onOpen: (key: string) => void;
};

type CaseFlowNode = Node<CaseFlowNodeData, "caseStep">;

function caseStepStatusLabel(status: CaseFlowStatus) {
  if (status === "active") return tr("In progress");
  if (status === "complete") return tr("Completed");
  if (status === "blocked") return tr("Execution issue");
  if (status === "skipped") return tr("Not executed");
  return tr("Ready to execute");
}

function CaseFlowNodeView({ data }: NodeProps<CaseFlowNode>) {
  const { step, index, status, onOpen } = data;

  return (
    <article className={`case-flow-node ${status} node-${step.kind}`}>
      <Handle type="target" position={Position.Left} id="target-left" />
      <Handle type="target" position={Position.Top} id="target-top" />
      <Handle type="source" position={Position.Right} id="source-right" />
      <Handle type="source" position={Position.Bottom} id="source-bottom" />
      <button
        type="button"
        className="case-flow-node-summary nodrag nopan"
        aria-haspopup="dialog"
        onPointerDown={(event) => event.stopPropagation()}
        onClick={(event) => {
          event.stopPropagation();
          onOpen(step.key);
        }}
      >
        <span className="case-flow-node-index">
          {status === "complete" ? "✓" : String(index + 1).padStart(2, "0")}
        </span>
        <span className="case-flow-node-copy">
          <strong>{step.label}</strong>
          <small className="case-flow-node-meta">
            <em className={`case-flow-node-kind kind-${step.kind}`}>
              {caseStepKindLabel(step.kind)}
            </em>
            <span>{caseStepStatusLabel(status)}</span>
          </small>
        </span>
        <span className="case-flow-node-chevron" aria-hidden="true">
          ↗
        </span>
      </button>
    </article>
  );
}

const caseFlowNodeTypes = { caseStep: CaseFlowNodeView };

function CaseFlowViewportFit({ signature }: { signature: string }) {
  const { fitView } = useReactFlow();

  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      fitView({ padding: 0.16, minZoom: 0.45, maxZoom: 1 });
    });
    return () => cancelAnimationFrame(frame);
  }, [fitView, signature]);

  return null;
}

async function copyCaseFlowText(text: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "true");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) throw new Error("clipboard unavailable");
}

function CaseFlowHttpEvidenceBlock({
  label,
  evidence,
  response,
}: {
  label: string;
  evidence: CaseFlowHttpEvidence;
  response: boolean;
}) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "error">("idle");

  const copy = async () => {
    try {
      await copyCaseFlowText(formatHttpEvidence(evidence, response));
      setCopyState("copied");
      window.setTimeout(() => setCopyState("idle"), 1600);
    } catch {
      setCopyState("error");
    }
  };

  return (
    <div className="case-flow-node-http-block">
      <div className="case-flow-node-http-block-head">
        <strong>{label}</strong>
        <button
          type="button"
          className="case-flow-copy-button"
          onClick={() => void copy()}
        >
          {copyState === "error" ? tr("Copy failed") : copyState === "copied" ? tr("Copied") : tr("Copy")}
        </button>
      </div>
      <pre>{formatHttpEvidence(evidence, response)}</pre>
    </div>
  );
}

function CaseFlowDetailsModal({
  steps,
  stepIndex,
  status,
  onClose,
  onStepChange,
}: {
  steps: CaseFlowStep[];
  stepIndex: number;
  status: CaseFlowStatus;
  onClose: () => void;
  onStepChange: (index: number) => void;
}) {
  const step = steps[stepIndex];

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
      if (event.key === "ArrowLeft" && stepIndex > 0) onStepChange(stepIndex - 1);
      if (event.key === "ArrowRight" && stepIndex < steps.length - 1) {
        onStepChange(stepIndex + 1);
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose, onStepChange, stepIndex, steps.length]);

  if (!step) return null;
  return (
    <div
      className="case-flow-details-backdrop"
      role="presentation"
      onMouseDown={onClose}
    >
      <section
        className="case-flow-details-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="case-flow-details-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="case-flow-details-header">
          <div>
            <p className="section-kicker">STEP {String(stepIndex + 1).padStart(2, "0")}</p>
            <h2 id="case-flow-details-title">{step.label}</h2>
            <div className="case-flow-details-meta">
              <em className={`case-flow-node-kind kind-${step.kind}`}>{caseStepKindLabel(step.kind)}</em>
              <span>{caseStepStatusLabel(status)}</span>
            </div>
          </div>
          <button
            type="button"
            className="case-flow-details-close"
            aria-label={tr("Close step details")}
            onClick={onClose}
          >
            ×
          </button>
        </header>
        <div className="case-flow-details-body">
          <dl className="case-flow-details-list">
            <div>
              <dt>{tr("Input")}</dt>
              <dd>{step.input}</dd>
            </div>
            <div>
              <dt>{tr("Action")}</dt>
              <dd>{step.action}</dd>
            </div>
            <div>
              <dt>{tr("Output")}</dt>
              <dd>{step.output}</dd>
            </div>
            <div>
              <dt>{tr("Assertion")}</dt>
              <dd>{step.assertion}</dd>
            </div>
          </dl>
          {(step.request || step.response) && (
            <div className="case-flow-node-http">
              <span>{tr("Actual API evidence")}</span>
              {step.request && (
                <CaseFlowHttpEvidenceBlock
                  label={tr("Request parameters")}
                  evidence={step.request}
                  response={false}
                />
              )}
              {step.response && (
                <CaseFlowHttpEvidenceBlock
                  label={tr("Response parameters")}
                  evidence={step.response}
                  response
                />
              )}
            </div>
          )}
        </div>
        <footer className="case-flow-details-footer">
          <button
            type="button"
            className="case-flow-detail-nav"
            disabled={stepIndex === 0}
            onClick={() => onStepChange(stepIndex - 1)}
          >
            {tr("← Previous")}
          </button>
          <span>{stepIndex + 1} / {steps.length}</span>
          <button
            type="button"
            className="case-flow-detail-nav"
            disabled={stepIndex === steps.length - 1}
            onClick={() => onStepChange(stepIndex + 1)}
          >
            {tr("Next →")}
          </button>
        </footer>
      </section>
    </div>
  );
}

function TestCaseFlow({
  detail,
  cleanupEnabled,
  execution,
  executionComplete,
  executionFailed,
  compact = false,
}: {
  detail: CaseReviewDetail;
  cleanupEnabled: boolean;
  execution: ExecutionPollDto | null;
  executionComplete: boolean;
  executionFailed: boolean;
  compact?: boolean;
}) {
  const cases = useMemo(() => buildCaseFlowCases(detail, execution), [detail, execution]);
  const firstCaseId = cases[0]?.id ?? null;
  const [selectedCaseId, setSelectedCaseId] = useState<string | null>(() => firstCaseId);
  const selectedCase =
    cases.find((item) => item.id === selectedCaseId) ?? cases[0] ?? null;
  const steps = selectedCase?.steps ?? [];
  const [selectedStepKey, setSelectedStepKey] = useState<string | null>(null);
  const caseSignature = cases
    .map((item) => `${item.id}:${item.steps.map((step) => step.key).join(",")}`)
    .join("|");
  const stepSignature = steps.map((step) => step.key).join(",");
  useEffect(() => {
    const nextCase = cases.find((item) => item.id === selectedCaseId) ?? cases[0] ?? null;
    if (nextCase?.id !== selectedCaseId) {
      setSelectedCaseId(nextCase?.id ?? null);
    }
    if (!nextCase || !nextCase.steps.some((step) => step.key === selectedStepKey)) {
      setSelectedStepKey(null);
    }
  }, [caseSignature, selectedCaseId, selectedStepKey]);
  const flowSignature = `${selectedCase?.id ?? "empty"}:${stepSignature}`;
  const selectedStepIndex = steps.findIndex((step) => step.key === selectedStepKey);
  const selectedStep = selectedStepIndex >= 0 ? steps[selectedStepIndex] : null;
  const observedStepIds = execution
    ? parseObservedStepIds(`${execution.stdout_tail}\n${execution.stderr_tail}`)
    : [];
  const observed = observedStepIds.length > 0
    ? steps.filter((step) => observedStepIds.includes(step.key)).length
    : null;
  const activeStep = steps.find((step, index) =>
    caseStepStatus({
      step,
      stepKey: step.key,
      index,
      total: steps.length,
      execution,
      executionComplete,
      executionFailed,
    }) === "active",
  );
  const caseStatuses = cases.map((item) => ({
    ...item,
    status: caseFlowCaseStatus(item, execution, executionComplete, executionFailed),
  }));
  const flowNodes = useMemo<CaseFlowNode[]>(() => {
    const statusFor = (step: CaseFlowStep, index: number) =>
      caseStepStatus({
        step,
        stepKey: step.key,
        index,
        total: steps.length,
        execution,
        executionComplete,
        executionFailed,
      });
    const collapsedRowHeight = 150;
    const rowTops: number[] = [];
    let top = 0;
    for (let row = 0; row < Math.ceil(steps.length / 2); row += 1) {
      rowTops.push(top);
      top += collapsedRowHeight;
    }
    return steps.map((step, index) => {
      const status = statusFor(step, index);
      return {
        id: step.key,
        type: "caseStep",
        position: {
          x: index % 2 === 0 ? 0 : 520,
          y: rowTops[Math.floor(index / 2)] ?? 0,
        },
        data: {
          step,
          index,
          status,
          onOpen: (key: string) => setSelectedStepKey(key),
        },
      };
    });
  }, [execution, executionComplete, executionFailed, steps]);
  const flowEdges = useMemo<Edge[]>(
    () =>
      steps.slice(0, -1).map((step, index) => {
        const across = index % 2 === 0;
        const status = caseStepStatus({
          step,
          stepKey: step.key,
          index,
          total: steps.length,
          execution,
          executionComplete,
          executionFailed,
        });
        return {
          id: `${step.key}->${steps[index + 1].key}`,
          source: step.key,
          target: steps[index + 1].key,
          sourceHandle: across ? "source-right" : "source-bottom",
          targetHandle: across ? "target-left" : "target-top",
          type: "smoothstep",
          animated: status === "active",
          markerEnd: {
            type: MarkerType.ArrowClosed,
            width: 12,
            height: 12,
            color: status === "complete" ? "#a9c77c" : "#697761",
          },
          zIndex: 1,
          className: `case-flow-edge ${status}`,
        };
      }),
    [execution, executionComplete, executionFailed, steps],
  );
  return (
    <section className={compact ? "case-flow compact" : "case-flow"}>
      <div className="case-flow-head">
        <div>
          <p className="section-kicker">CASE FLOW</p>
          <h3>{tr("Case execution steps")}</h3>
          <p className="case-flow-summary">
            {steps.length > 0
              ? `${selectedCase && cases.length > 1 ? `${selectedCase.label} · ` : ""}${tr("{count} business actions", { count: steps.length })}${activeStep ? ` · ${tr("Current: {step}", { step: activeStep.label })}` : ""}`
              : tr("Waiting for the generation service to emit a business-step contract")}
          </p>
        </div>
        <StateMark
          tone={executionFailed ? "danger" : executionComplete ? "success" : execution ? "active" : "neutral"}
          label={
            executionComplete
              ? executionFailed
                ? tr("Execution has issues")
                : tr("Execution complete")
              : execution
                ? tr("Running")
                : tr("Ready to execute")
          }
        />
      </div>
      {cases.length > 0 ? (
        <div className="case-flow-layout">
          <aside className="case-flow-case-menu">
            <div className="case-flow-case-menu-heading">
              <span className="section-kicker">TEST SUITE</span>
              <div className="case-flow-heading-title">
                <strong>{tr("{count} business cases", { count: cases.length })}</strong>
                <HelpHint message={tr("Select a scenario to view its complete execution steps.")} />
              </div>
            </div>
            <nav className="case-flow-case-list" role="tablist" aria-label={tr("Test case list")}>
              {caseStatuses.map((item, index) => (
                <button
                  key={item.id}
                  type="button"
                  role="tab"
                  aria-selected={selectedCase?.id === item.id}
                  className={selectedCase?.id === item.id ? "active" : ""}
                  onClick={() => {
                    setSelectedCaseId(item.id);
                    setSelectedStepKey(null);
                  }}
                >
                  <span>{String(index + 1).padStart(2, "0")}</span>
                  <strong>{item.label}</strong>
                  <small>{tr("{count} steps", { count: item.steps.length })} · {caseStepStatusLabel(item.status)}</small>
                </button>
              ))}
            </nav>
          </aside>
          <div className="case-flow-main">
            <div className="case-flow-selected-heading">
              <div>
                <span className="section-kicker">SELECTED CASE</span>
                <strong>{selectedCase?.label ?? tr("No case selected")}</strong>
              </div>
              <span>{tr("{count} steps", { count: steps.length })}</span>
            </div>
            <div className="case-flow-canvas" role="list" aria-label={tr("Case execution flowchart")}>
              <ReactFlow
                key={flowSignature}
                nodes={flowNodes}
                edges={flowEdges}
                nodeTypes={caseFlowNodeTypes}
                fitView
                fitViewOptions={{ padding: 0.16, minZoom: 0.45, maxZoom: 1 }}
                onInit={(instance) => {
                  requestAnimationFrame(() => {
                    instance.fitView({ padding: 0.16, minZoom: 0.45, maxZoom: 1 });
                  });
                }}
                nodesDraggable={false}
                nodesConnectable={false}
                elementsSelectable={false}
                zoomOnDoubleClick={false}
                proOptions={{ hideAttribution: true }}
              >
                <Background color="#252a23" gap={18} size={1} />
                <Controls showInteractive={false} />
                <CaseFlowViewportFit signature={flowSignature} />
              </ReactFlow>
            </div>
          </div>
        </div>
      ) : (
        <div className="case-flow-empty">
          <strong>{tr("No business steps to display")}</strong>
          <p>
            {tr("The generation service must emit TESTHARBOR_CASE and TESTHARBOR_STEP contracts in candidate tests. Legacy candidates appear as one complete business flow.")}
          </p>
        </div>
      )}
      <div className="case-flow-foot">
        <span>{tr("Intended tests: {count}", { count: detail.intended_count || tr("Pending collection") })}</span>
        <span>{tr("Completed steps: {count}", { count: observed == null ? "—" : `${observed}/${steps.length}` })}</span>
        <span>{tr("Cleanup policy: {policy}", { policy: cleanupEnabled ? tr("Clean up after execution") : tr("Retain isolated data") })}</span>
      </div>
      {selectedStep && (
        <CaseFlowDetailsModal
          steps={steps}
          stepIndex={selectedStepIndex}
          status={caseStepStatus({
            step: selectedStep,
            stepKey: selectedStep.key,
            index: selectedStepIndex,
            total: steps.length,
            execution,
            executionComplete,
            executionFailed,
          })}
          onClose={() => setSelectedStepKey(null)}
          onStepChange={(index) => setSelectedStepKey(steps[index]?.key ?? null)}
        />
      )}
    </section>
  );
}

function clipConsoleText(value: string, maxLength = 1200) {
  const text = value.replace(/\s+/g, " ").trim();
  return text.length > maxLength ? `${text.slice(0, maxLength)}…` : text;
}

function claudeTextFrom(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    return value
      .map((item) => claudeTextFrom(item))
      .filter(Boolean)
      .join("");
  }
  if (!value || typeof value !== "object") return "";
  const object = value as Record<string, unknown>;
  if (typeof object.text === "string") return object.text;
  if (object.content !== undefined) return claudeTextFrom(object.content);
  if (object.message !== undefined) return claudeTextFrom(object.message);
  if (object.delta !== undefined) return claudeTextFrom(object.delta);
  return "";
}

function claudeToolUses(value: unknown) {
  const message = value && typeof value === "object"
    ? (value as Record<string, unknown>).message
    : undefined;
  const content = message && typeof message === "object"
    ? (message as Record<string, unknown>).content
    : undefined;
  if (!Array.isArray(content)) return [];
  return content
    .filter((block): block is Record<string, unknown> => Boolean(block && typeof block === "object"))
    .filter((block) => block.type === "tool_use")
    .map((block) => ({
      name: typeof block.name === "string" ? block.name : "工具",
      input: block.input,
    }));
}

function claudeToolResultSummary(value: unknown) {
  const message = value && typeof value === "object"
    ? (value as Record<string, unknown>).message
    : undefined;
  const content = message && typeof message === "object"
    ? (message as Record<string, unknown>).content
    : undefined;
  if (!Array.isArray(content)) return "";
  const results = content.filter(
    (block): block is Record<string, unknown> => Boolean(block && typeof block === "object") && block.type === "tool_result",
  );
  return results.length > 0 ? `工具已返回 ${results.length} 项结果` : "";
}

function formatBytes(value: number) {
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  if (value < 1024) return `${Math.round(value)} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function formatCodexConsole(raw: string) {
  if (!raw.trim()) return "正在启动生成服务…";
  const rendered: string[] = [];
  let streamKind: "thought" | "text" | null = null;
  let streamBuffer = "";
  let claudeStreamBuffer = "";
  const claudeAnnouncedTools = new Set<string>();

  const flushStream = () => {
    if (!streamKind || !streamBuffer) return;
    rendered.push(streamKind === "thought" ? `· ${streamBuffer}` : streamBuffer);
    streamKind = null;
    streamBuffer = "";
  };

  const flushClaudeStream = () => {
    const text = claudeStreamBuffer.trim();
    if (text) rendered.push(text);
    claudeStreamBuffer = "";
  };

  const appendClaudeAssistantText = (text: string) => {
    if (!text) return;
    const current = claudeStreamBuffer;
    if (!current) {
      claudeStreamBuffer = text;
    } else if (text.startsWith(current) || current.startsWith(text)) {
      claudeStreamBuffer = text.length >= current.length ? text : current;
    } else {
      flushClaudeStream();
      rendered.push(text);
    }
  };

  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const event = JSON.parse(trimmed) as Record<string, unknown>;
      const type = String(event.type ?? "event");
      const item =
        event.item && typeof event.item === "object"
          ? (event.item as Record<string, unknown>)
          : null;
      const itemType = item ? String(item.type ?? "") : "";
      const message =
        typeof event.message === "string"
          ? event.message
          : typeof event.data === "string"
            ? event.data
          : item && typeof item.text === "string"
            ? item.text
            : "";

      // Claude Code's stream-json is an event protocol, not a console log.
      // Render the useful parts of system/assistant/tool/result events and
      // hide the raw event labels that otherwise appear as `. system` etc.
      const isClaudeEvent =
        type === "system" ||
        type === "assistant" ||
        type === "user" ||
        type === "result" ||
        type === "stream_event";
      if (isClaudeEvent) {
        flushStream();
        if (type === "system") {
          const subtype = typeof event.subtype === "string" ? event.subtype : "";
          if (subtype === "init") {
            const model = typeof event.model === "string" ? `（${event.model}）` : "";
            rendered.push(`◆ Claude Code 已启动${model}`);
          } else if (subtype) {
            rendered.push(`◆ Claude Code：${subtype}`);
          }
        } else if (type === "stream_event") {
          const streamEvent =
            event.event && typeof event.event === "object"
              ? (event.event as Record<string, unknown>)
              : null;
          const streamType = String(streamEvent?.type ?? "");
          if (streamType === "content_block_delta") {
            const delta = streamEvent?.delta;
            const deltaText = claudeTextFrom(delta);
            if (delta && typeof delta === "object" && (delta as Record<string, unknown>).type === "text_delta") {
              claudeStreamBuffer += deltaText;
            }
          } else if (streamType === "content_block_start") {
            const block = streamEvent?.content_block;
            if (block && typeof block === "object" && (block as Record<string, unknown>).type === "tool_use") {
              flushClaudeStream();
              const tool = block as Record<string, unknown>;
              const name = String(tool.name ?? "工具");
              claudeAnnouncedTools.add(name);
              rendered.push(`↳ 正在调用工具：${name}`);
            }
          }
        } else if (type === "assistant") {
          const assistantText = claudeTextFrom(event.message);
          appendClaudeAssistantText(assistantText);
          for (const tool of claudeToolUses(event)) {
            if (claudeAnnouncedTools.delete(tool.name)) continue;
            flushClaudeStream();
            const input = tool.input === undefined ? "" : ` · ${clipConsoleText(JSON.stringify(tool.input), 180)}`;
            rendered.push(`↳ 调用工具：${tool.name}${input}`);
          }
        } else if (type === "user") {
          flushClaudeStream();
          const summary = claudeToolResultSummary(event);
          if (summary) rendered.push(`↳ ${summary}`);
        } else if (type === "result") {
          const resultText = typeof event.result === "string" ? event.result : "";
          if (event.is_error === true) {
            rendered.push(`! Claude Code 生成失败${resultText ? `：${clipConsoleText(resultText)}` : ""}`);
          } else {
            if (!claudeStreamBuffer && resultText) rendered.push(clipConsoleText(resultText));
            rendered.push("◆ Claude Code 生成完成");
          }
        }
        continue;
      }

      if (type === "thought" || type === "text") {
        if (streamKind !== type) flushStream();
        streamKind = type;
        streamBuffer += message;
        continue;
      }

      flushStream();
      if (type === "testharbor.status") {
        rendered.push(`◆ ${message}`);
      } else if (type === "testharbor.agent_call") {
        const promptBytes = Number(event.prompt_bytes ?? 0);
        const outputBytes = Number(event.stdout_bytes ?? 0);
        const durationMs = Number(event.duration_ms ?? 0);
        const program = String(event.program ?? "CLI");
        rendered.push(
          `◆ ${program} 调用完成 · 输入 ${formatBytes(promptBytes)} · 输出 ${formatBytes(outputBytes)} · ${Math.round(durationMs / 1000)}s`,
        );
      } else if (type === "thread.started") {
        rendered.push("◆ 会话已建立");
      } else if (type === "turn.started") {
        rendered.push("◆ 开始分析仓库");
      } else if (type === "turn.completed") {
        rendered.push("◆ 本轮分析完成");
      } else if (type.includes("error")) {
        rendered.push(`! ${message || JSON.stringify(event)}`);
      } else if (itemType === "command_execution") {
        const command = String(item?.command ?? "");
        if (type.endsWith("started")) {
          rendered.push(`$ ${command}`);
        } else {
          const output = String(item?.aggregated_output ?? "").trim();
          const code = item?.exit_code;
          if (output) rendered.push(output);
          rendered.push(`↳ exit ${code ?? "—"}`);
        }
      } else if (itemType === "agent_message" || itemType === "reasoning") {
        if (message) rendered.push(message);
      } else if (type === "end") {
        rendered.push("◆ 生成服务已完成");
      } else if (message) {
        rendered.push(message);
      } else {
        rendered.push(`· ${type}`);
      }
    } catch {
      // The last read can end in the middle of a JSONL record while the
      // process is still writing. Wait for the next poll instead of showing
      // a broken JSON fragment in the console.
      if (trimmed.startsWith("{") && !trimmed.endsWith("}")) continue;
      flushStream();
      rendered.push(line);
    }
  }
  flushClaudeStream();
  flushStream();
  return rendered.slice(-160).join("\n");
}

function App() {
  const [language, setLanguage] = useState<Language>("en");
  setActiveLanguage(language);
  const t = (message: string, params?: Record<string, string | number>) =>
    translate(language, message, params);
  const [tab, setTab] = useState<Tab>("project");
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [operationBusy, setBusy] = useState(false);
  const [globalActivity, setActivity] = useState<Activity | null>(null);
  const [runActivities, setRunActivities] = useState<Record<string, Activity>>({});

  // Project
  const [pathInput, setPathInput] = useState("");
  const [repo, setRepo] = useState<GitRepoInfoDto | null>(null);
  const [projectReadiness, setProjectReadiness] = useState<ProjectReadiness | null>(null);
  const [project, setProject] = useState<ProjectSummary | null>(null);
  const [recent, setRecent] = useState<ProjectSummary[]>([]);
  const [sidebarHistory, setSidebarHistory] = useState<RunHistoryItem[]>([]);
  const [runId, setRunId] = useState<string | null>(null);
  const activeRunIdRef = useRef<string | null>(null);
  const currentRunActivity = runId ? runActivities[runId] ?? null : null;
  const backgroundActivities = Object.values(runActivities).filter(
    (item) => item.run_id !== runId,
  );
  const backgroundActivity = backgroundActivities[0] ?? null;
  const activity = globalActivity ?? backgroundActivity;
  const busy = operationBusy || currentRunActivity !== null;
  const [projectsLoaded, setProjectsLoaded] = useState(false);
  const [historyLoaded, setHistoryLoaded] = useState(false);
  const didRestoreWorkspace = useRef(false);

  // Run workflow messages
  const [log, setLog] = useState<string[]>([]);
  const [objective, setObjective] = useState(DEFAULT_TEST_OBJECTIVE);
  const [criteriaText, setCriteriaText] = useState(DEFAULT_ACCEPTANCE_CRITERIA);
  const [sourceAnchorsText] = useState("");
  const [generationScopeText] = useState("");
  const [pastedSpecLabel] = useState("补充说明");
  const [pastedSpecBody] = useState("");
  const [repoMarkdownText] = useState("");
  const [testStrategy, setTestStrategy] = useState("e2e_api_flow");
  const [cleanupTestData, setCleanupTestData] = useState(true);
  const [testEnvironment, setTestEnvironment] =
    useState<TestEnvironmentInput>(emptyTestEnvironment);
  const [approvedCase, setApprovedCase] = useState(false);
  const [trusted, setTrusted] = useState(false);
  const [acceptInfo, setAcceptInfo] = useState<string | null>(null);
  const [analysisInfo, setAnalysisInfo] = useState<string | null>(null);
  const [resultReviewInfo, setResultReviewInfo] = useState<string | null>(null);
  const [execution, setExecution] = useState<ExecutionPollDto | null>(null);
  const [executionComplete, setExecutionComplete] = useState(false);
  const [canAcceptPass, setCanAcceptPass] = useState(false);
  const [readyToApply, setReadyToApply] = useState(false);
  const cancelRequestedRuns = useRef<Set<string>>(new Set());
  const generationConsoleRef = useRef<HTMLPreElement>(null);
  const [caseDetail, setCaseDetail] = useState<CaseReviewDetail | null>(null);
  const [projectCases, setProjectCases] = useState<ProjectCaseRecord[]>([]);
  const [projectCasesLoading, setProjectCasesLoading] = useState(false);
  const [generationProgress, setGenerationProgress] =
    useState<GenerationProgressDto>({
      stdout_tail: "",
      stderr_tail: "",
    });
  const [changeRequest, setChangeRequest] = useState("");
  const [generationRetryAvailable, setGenerationRetryAvailable] = useState(false);
  const [riskDrafts, setRiskDrafts] = useState<Record<string, string>>({});
  const [reviewDecisionOpen, setReviewDecisionOpen] = useState(false);

  // Settings
  const [usage, setUsage] = useState<DiskUsage | null>(null);
  const [settingsMsg, setSettingsMsg] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    () => window.localStorage.getItem("testharbor:sidebar-collapsed") === "true",
  );
  const [codex, setCodex] = useState<CodexEnvironment | null>(null);
  const [claude, setClaude] = useState<ClaudeEnvironment | null>(null);
  const [grok, setGrok] = useState<GrokEnvironment | null>(null);
  const [codexModels, setCodexModels] = useState<string[]>([]);
  const [claudeModels, setClaudeModels] = useState<string[]>([]);
  const [grokModels, setGrokModels] = useState<string[]>([]);
  const [codexDefaultModel, setCodexDefaultModel] = useState<string | null>(null);
  const [claudeDefaultModel, setClaudeDefaultModel] = useState<string | null>(null);
  const [grokDefaultModel, setGrokDefaultModel] = useState<string | null>(null);
  const [agentPreferences, setAgentPreferences] = useState<AgentPreferences>({
    enabled_modes: ["codex", "claude", "grok"],
  });
  const [agentMode, setAgentMode] = useState("codex");
  const [agentModel, setAgentModel] = useState("");
  const [reuseAgentSession, setReuseAgentSession] = useState(false);
  const [agentModelSaving, setAgentModelSaving] = useState(false);
  const [recoveryBanner, setRecoveryBanner] = useState<string | null>(null);
  const [events, setEvents] = useState<{ event_type: string; created_at: string }[]>(
    [],
  );

  useEffect(() => {
    document.documentElement.lang = language;
  }, [language]);

  // Once a run has been generated, the persisted provider is authoritative.
  // This prevents the model selector from falling back to the initial Codex
  // state while the case-review detail is loading or being restored.
  const configuredAgentMode =
    caseDetail?.agent_mode === "grok" ||
    caseDetail?.agent_mode === "claude" ||
    caseDetail?.agent_mode === "codex"
      ? caseDetail.agent_mode
      : agentMode;
  const selectedAgent =
    configuredAgentMode === "grok"
      ? grok
      : configuredAgentMode === "claude"
        ? claude
        : configuredAgentMode === "codex"
          ? codex
          : null;
  const agentReady = selectedAgent?.ready ?? false;
  const agentLabel =
    configuredAgentMode === "grok"
      ? "Grok CLI"
      : configuredAgentMode === "claude"
        ? "Claude Code CLI"
        : configuredAgentMode === "codex"
          ? "Codex CLI"
          : "未配置生成服务";
  const availableAgentOptions: SelectOption[] = [
    { value: "codex", label: "Codex CLI", enabled: Boolean(codex?.ready) },
    { value: "claude", label: "Claude Code CLI", enabled: Boolean(claude?.ready) },
    { value: "grok", label: "Grok CLI", enabled: Boolean(grok?.ready) },
  ].filter(
    (option) =>
      agentPreferences.enabled_modes.includes(option.value) && option.enabled,
  );
  const taskAgentOptions =
    caseDetail && configuredAgentMode && configuredAgentMode !== "mock" &&
    !availableAgentOptions.some((option) => option.value === configuredAgentMode)
      ? [
          ...availableAgentOptions,
          {
            value: configuredAgentMode,
            label:
              configuredAgentMode === "claude"
                ? "Claude Code CLI"
                : configuredAgentMode === "grok"
                  ? "Grok CLI"
                  : "Codex CLI",
            enabled: true,
          },
        ]
      : availableAgentOptions;
  const makeModelOptions = (
    models: string[],
    defaultModel: string | null,
    selectedModel = "",
  ): SelectOption[] => [
    {
      value: "",
      label: defaultModel ? `CLI 默认（${defaultModel}）` : "CLI 默认模型",
      enabled: true,
    },
    ...Array.from(
      new Set([...models, ...(selectedModel.trim() ? [selectedModel] : [])]),
    ).map((model) => ({ value: model, label: model, enabled: true })),
  ];
  const codexModelOptions = makeModelOptions(
    codexModels,
    codexDefaultModel,
    configuredAgentMode === "codex" ? agentModel : "",
  );
  const claudeModelOptions = makeModelOptions(
    claudeModels,
    claudeDefaultModel,
    configuredAgentMode === "claude" ? agentModel : "",
  );
  const grokModelOptions = makeModelOptions(
    grokModels,
    grokDefaultModel,
    configuredAgentMode === "grok" ? agentModel : "",
  );
  const agentModelOptions =
    configuredAgentMode === "grok"
      ? grokModelOptions
      : configuredAgentMode === "claude"
        ? claudeModelOptions
        : codexModelOptions;

  const refreshCodexModels = useCallback(async () => {
    try {
      const catalog = await invoke<CliModelCatalog>("get_codex_models");
      setCodexModels(catalog.models);
      setCodexDefaultModel(catalog.default_model);
    } catch {
      setCodexModels([]);
      setCodexDefaultModel(null);
    }
  }, []);

  const refreshClaudeModels = useCallback(async () => {
    try {
      const catalog = await invoke<CliModelCatalog>("get_claude_models");
      setClaudeModels(catalog.models);
      setClaudeDefaultModel(catalog.default_model);
    } catch {
      setClaudeModels([]);
      setClaudeDefaultModel(null);
    }
  }, []);

  const refreshGrokModels = useCallback(async () => {
    try {
      const catalog = await invoke<CliModelCatalog>("get_grok_models");
      setGrokModels(catalog.models);
      setGrokDefaultModel(catalog.default_model);
    } catch {
      setGrokModels([]);
      setGrokDefaultModel(null);
    }
  }, []);

  const pushLog = (m: string) => setLog((prev) => [m, ...prev].slice(0, 40));

  useEffect(() => {
    activeRunIdRef.current = runId;
  }, [runId]);

  function isActiveRun(id: string) {
    return activeRunIdRef.current === id;
  }

  function clearRunActivity(id: string) {
    setRunActivities((current) => {
      if (!current[id]) return current;
      const next = { ...current };
      delete next[id];
      return next;
    });
  }

  function setRunActivity(id: string, next: Activity) {
    setRunActivities((current) => ({
      ...current,
      [id]: { ...next, run_id: id },
    }));
  }

  function toggleSidebar() {
    setSidebarCollapsed((previous) => {
      const next = !previous;
      window.localStorage.setItem("testharbor:sidebar-collapsed", String(next));
      return next;
    });
  }

  const refreshRecent = useCallback(async () => {
    try {
      setRecent(await invoke<ProjectSummary[]>("list_projects"));
    } catch {
      /* ignore */
    } finally {
      setProjectsLoaded(true);
    }
  }, []);

  const refreshSidebarHistory = useCallback(async () => {
    try {
      setSidebarHistory(
        await invoke<RunHistoryItem[]>("list_history", {
          projectId: null,
          filter: "all",
          limit: 200,
        }),
      );
    } catch {
      /* ignore */
    } finally {
      setHistoryLoaded(true);
    }
  }, []);

  const refreshHistory = useCallback(async () => {
    try {
      setSidebarHistory(
        await invoke<RunHistoryItem[]>("list_history", {
          projectId: null,
          filter: "all",
          limit: 200,
        }),
      );
      setHistoryLoaded(true);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    const projectRuns = project
      ? sidebarHistory.filter((item) => item.project_id === project.id)
      : [];
    if (!project || projectRuns.length === 0) {
      setProjectCases([]);
      setProjectCasesLoading(false);
      return () => {
        disposed = true;
      };
    }

    setProjectCasesLoading(true);
    void Promise.all(
      projectRuns.map(async (run) => {
        try {
          const detail = await invoke<CaseReviewDetail>("get_case_review_detail", {
            runId: run.run_id,
          });
          return buildCaseFlowCases(detail, null).map((item) => ({
            key: `${run.run_id}:${item.id}`,
            case_id: item.id,
            label: item.label,
            objective: item.objective || run.objective,
            step_count: item.steps.length,
            approved: detail.approved,
            can_rerun:
              detail.approved &&
              ["PendingResultReview", "NeedsHuman", "ReadyToApply"].includes(
                detail.workflow_state,
              ),
            run,
          }));
        } catch {
          return [];
        }
      }),
    )
      .then((groups) => {
        if (disposed) return;
        setProjectCases(
          groups
            .flat()
            .sort((left, right) => {
              const byRun = right.run.updated_at.localeCompare(left.run.updated_at);
              return byRun || left.label.localeCompare(right.label, "zh-CN");
            }),
        );
      })
      .finally(() => {
        if (!disposed) setProjectCasesLoading(false);
      });

    return () => {
      disposed = true;
    };
  }, [project?.id, sidebarHistory]);

  useEffect(() => {
    invoke<{
      info: AppInfo;
      last_recovery: RecoveryReport | null;
      ui_preferences: UiPreferences;
      agent_mode_default: string;
      agent_preferences: AgentPreferences;
      codex: CodexEnvironment;
      claude: ClaudeEnvironment;
      grok: GrokEnvironment;
    }>("get_app_info_ext")
      .then((ext) => {
        setInfo(ext.info);
        setLanguage(normalizeLanguage(ext.ui_preferences.language));
        setCodex(ext.codex);
        setClaude(ext.claude);
        setGrok(ext.grok);
        setAgentPreferences(ext.agent_preferences);
        setAgentMode(
          selectConfiguredAgentMode(ext.agent_mode_default, ext.agent_preferences, {
            codex: ext.codex,
            claude: ext.claude,
            grok: ext.grok,
          }),
        );
        if (ext.codex.ready) void refreshCodexModels();
        if (ext.claude.ready) void refreshClaudeModels();
        if (ext.grok.ready) void refreshGrokModels();
        setRecoveryBanner(recoveryNotice(ext.last_recovery));
      })
      .catch(() => {
        invoke<AppInfo>("get_app_info")
          .then(setInfo)
          .catch((e) => setError(String(e)));
      });
    refreshRecent();
    refreshSidebarHistory();
  }, [
    refreshClaudeModels,
    refreshCodexModels,
    refreshGrokModels,
    refreshRecent,
    refreshSidebarHistory,
  ]);

  useEffect(() => {
    if (!projectsLoaded || !historyLoaded || didRestoreWorkspace.current) return;
    didRestoreWorkspace.current = true;

    const savedRunId = window.localStorage.getItem("testharbor:last-run");
    const savedProjectId = window.localStorage.getItem("testharbor:last-project");
    const savedRun = sidebarHistory.find((item) => item.run_id === savedRunId);
    if (savedRun) {
      void openHistoryRun(savedRun);
      return;
    }

    const savedProject =
      recent.find((item) => item.id === savedProjectId) ?? recent[0] ?? null;
    if (savedProject) {
      void selectProject(savedProject, false);
    }
  }, [historyLoaded, projectsLoaded, recent, sidebarHistory]);

  async function refreshCodexEnvironment() {
    setError(null);
    try {
      const ext = await invoke<{
        info: AppInfo;
        last_recovery: null;
        ui_preferences: UiPreferences;
        agent_mode_default: string;
        agent_preferences: AgentPreferences;
        codex: CodexEnvironment;
        claude: ClaudeEnvironment;
        grok: GrokEnvironment;
      }>("get_app_info_ext");
      setInfo(ext.info);
      setLanguage(normalizeLanguage(ext.ui_preferences.language));
      setCodex(ext.codex);
      setClaude(ext.claude);
      setGrok(ext.grok);
      setAgentPreferences(ext.agent_preferences);
      await Promise.all([
        ext.codex.ready ? refreshCodexModels() : Promise.resolve(),
        ext.claude.ready ? refreshClaudeModels() : Promise.resolve(),
        ext.grok.ready ? refreshGrokModels() : Promise.resolve(),
      ]);
      const selected =
        agentMode === "grok"
          ? ext.grok
          : agentMode === "claude"
            ? ext.claude
            : ext.codex;
      if (!selected.ready) {
        setError(
          operationErrorMessage(
            selected.error ?? "生成服务暂不可用，请完成配置后重试。",
          ),
        );
      }
    } catch (e) {
      setError(operationErrorMessage(e));
    }
  }

  const refreshEvents = useCallback(async (id: string) => {
    try {
      const ev = await invoke<{ event_type: string; created_at: string }[]>(
        "list_events",
        { runId: id, limit: 30 },
      );
      setEvents(ev);
    } catch {
      /* ignore */
    }
  }, []);

  const loadCaseReview = useCallback(async (id: string) => {
    try {
      const d = await invoke<CaseReviewDetail>("case_review_detail", {
        runId: id,
      });
      if (!isActiveRun(id)) return;
      setCaseDetail(d);
      setObjective(d.objective);
      setAgentModel(d.agent_model ?? "");
      setReuseAgentSession(d.reuse_agent_session);
      if (d.environment) {
        setTestEnvironment((previous) => ({
          ...previous,
          context: d.environment?.context ?? "",
          base_url: d.environment?.base_url ?? "",
          health_url: d.environment?.health_url ?? "",
          login_url: d.environment?.login_url ?? "",
          startup_command: d.environment?.startup_command ?? "",
          startup_cwd: d.environment?.startup_cwd ?? "",
        }));
      }
      if (d.agent_mode === "grok" || d.agent_mode === "claude" || d.agent_mode === "codex") {
        setAgentMode(d.agent_mode);
      }
      if (d.acceptance_criteria.length > 0) {
        setCriteriaText(d.acceptance_criteria.join("\n"));
      }
      setApprovedCase(d.approved);
      const drafts: Record<string, string> = {};
      for (const f of d.review.findings) {
        if (f.risk_reason) drafts[f.id] = f.risk_reason;
      }
      setRiskDrafts(drafts);
    } catch {
      if (isActiveRun(id)) {
        setCaseDetail(null);
      }
    }
  }, []);

  useEffect(() => {
    if (!settingsOpen) return;
    invoke<DiskUsage>("get_disk_usage")
      .then(setUsage)
      .catch((e) => setError(String(e)));
  }, [settingsOpen]);

  useEffect(() => {
    const consoleElement = generationConsoleRef.current;
    if (consoleElement) {
      consoleElement.scrollTop = consoleElement.scrollHeight;
    }
  }, [generationProgress.stdout_tail, generationProgress.stderr_tail]);

  async function openProjectPath(path: string, createRun: boolean) {
    setError(null);
    setBusy(true);
    setActivity({
      kind: "project",
      title: createRun ? "Creating test task" : "Reading repository",
      detail: "Check Git status and establish the local project record",
    });
    try {
      const opened = await invoke<OpenProjectResult>("open_repo_project", {
        path,
        createRun: false,
      });
      setProject(opened.project);
      setRepo(opened.repo);
      setProjectReadiness(opened.readiness);
      setPathInput(opened.project.root_path);
      window.localStorage.setItem("testharbor:last-project", opened.project.id);
      const createdRun = createRun
        ? await invoke<CreatedRunSummary>("create_project_run", {
            projectId: opened.project.id,
          })
        : null;
      if (createdRun) {
        resetNewRunForm();
        activeRunIdRef.current = createdRun.run_id;
        setRunId(createdRun.run_id);
        setCaseDetail(null);
        setAgentModel("");
        window.localStorage.setItem("testharbor:last-run", createdRun.run_id);
        setTab("run");
        pushLog(
          `Run ${createdRun.run_id.slice(0, 8)}… @ ${createdRun.base_commit.slice(0, 7)}`,
        );
      } else {
        setTab("project");
      }
      await Promise.all([refreshRecent(), refreshSidebarHistory()]);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  function resetNewRunForm() {
    setCaseDetail(null);
    setObjective(DEFAULT_TEST_OBJECTIVE);
    setCriteriaText(DEFAULT_ACCEPTANCE_CRITERIA);
    setTestEnvironment(emptyTestEnvironment());
    setTestStrategy("e2e_api_flow");
    setCleanupTestData(true);
    setAgentModel("");
    setReuseAgentSession(false);
    setApprovedCase(false);
    setTrusted(false);
    setAcceptInfo(null);
    setAnalysisInfo(null);
    setResultReviewInfo(null);
    setExecution(null);
    setExecutionComplete(false);
    setCanAcceptPass(false);
    setReadyToApply(false);
    setGenerationProgress({ stdout_tail: "", stderr_tail: "" });
    setGenerationRetryAvailable(false);
    setRiskDrafts({});
    setReviewDecisionOpen(false);
    setEvents([]);
    setLog([]);
  }

  async function pickAndOpen(createRun: boolean) {
    let path = pathInput.trim();
    if (!path || recent.some((item) => item.root_path === path)) {
      const selected = await open({
        directory: true,
        multiple: false,
        title: t("Choose Git repository"),
      });
      if (!selected) return;
      path = Array.isArray(selected) ? selected[0] : selected;
    }
    await openProjectPath(path, createRun);
  }

  async function selectProject(item: ProjectSummary, clearRun = true) {
    if (clearRun) {
      activeRunIdRef.current = null;
      setRunId(null);
      window.localStorage.removeItem("testharbor:last-run");
    }
    await openProjectPath(item.root_path, false);
  }

  async function createRunForProject(item: ProjectSummary | null) {
    if (item) {
      await openProjectPath(item.root_path, true);
      return;
    }
    setPathInput("");
    await pickAndOpen(true);
  }

  useEffect(() => {
    function handleNewTaskShortcut(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      const isEditing =
        target?.isContentEditable ||
        ["INPUT", "TEXTAREA", "SELECT"].includes(target?.tagName ?? "");
      if (
        !isEditing &&
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === "n" &&
        !operationBusy
      ) {
        event.preventDefault();
        void createRunForProject(project ?? recent[0] ?? null);
      }
      if (
        !isEditing &&
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === "b"
      ) {
        event.preventDefault();
        toggleSidebar();
      }
      if ((event.metaKey || event.ctrlKey) && event.key === ",") {
        event.preventDefault();
        setSettingsOpen(true);
      }
      if (event.key === "Escape" && settingsOpen) {
        setSettingsOpen(false);
      }
    }

    window.addEventListener("keydown", handleNewTaskShortcut);
    return () => window.removeEventListener("keydown", handleNewTaskShortcut);
  });

  async function openHistoryRun(item: RunHistoryItem) {
    activeRunIdRef.current = item.run_id;
    setRunId(item.run_id);
    window.localStorage.setItem("testharbor:last-run", item.run_id);
    window.localStorage.setItem("testharbor:last-project", item.project_id);
    setProject({
      id: item.project_id,
      name: item.project_name,
      root_path: item.project_root,
      git_remote: null,
    });
    setPathInput(item.project_root);
    setTab("run");
    setApprovedCase(false);
    setTrusted(false);
    setCaseDetail(null);
    setExecution(null);
    setExecutionComplete(false);
    setCanAcceptPass(false);
    setGenerationRetryAvailable(item.can_retry_generation);
    if (item.agent_mode === "grok" || item.agent_mode === "claude" || item.agent_mode === "codex") {
      setAgentMode(item.agent_mode);
    }
    setAgentModel(item.agent_model ?? "");
    if (item.objective && item.objective !== "(无目标)") {
      setObjective(item.objective);
    }
    if (item.acceptance_criteria.length > 0) {
      setCriteriaText(item.acceptance_criteria.join("\n"));
    }
    pushLog(`恢复 Run ${item.run_id.slice(0, 8)}… · ${item.resume_hint}`);
    try {
      const opened = await invoke<OpenProjectResult>("open_repo_project", {
        path: item.project_root,
        createRun: false,
      });
      setProject(opened.project);
      setRepo(opened.repo);
      setProjectReadiness(opened.readiness);
    } catch {
      setRepo(null);
      setProjectReadiness(null);
    }
    await loadCaseReview(item.run_id);
    try {
      const persisted = await invoke<ExecutionPollDto>("last_execution", {
        runId: item.run_id,
      });
      if (isActiveRun(item.run_id)) {
        applyCompletedExecution(persisted);
      }
    } catch {
      // This Run may not have reached execution yet or its report was cleaned up.
    }
    await refreshEvents(item.run_id);
  }

  async function deleteHistoryRun(item: RunHistoryItem) {
    if (busy) return;
    const title =
      item.objective && item.objective !== "(无目标)"
        ? item.objective
        : "新测试任务";
    const ok = window.confirm(
      `删除任务“${title}”？\n\n会删除该任务的本地记录、事件、生成产物和隔离 worktree，不会修改项目主仓。`,
    );
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await invoke("delete_task_run", { runId: item.run_id });
      setSidebarHistory((previous) =>
        previous.filter((historyItem) => historyItem.run_id !== item.run_id),
      );
      if (runId === item.run_id) {
        activeRunIdRef.current = null;
        setRunId(null);
        window.localStorage.removeItem("testharbor:last-run");
        setTab("project");
        setCaseDetail(null);
        setApprovedCase(false);
        setExecution(null);
        setExecutionComplete(false);
        setCanAcceptPass(false);
        setEvents([]);
      }
      pushLog(`已删除任务 ${item.run_id.slice(0, 8)}…`);
      await refreshSidebarHistory();
    } catch (e) {
      setError(operationErrorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function collectInventoryInBackground(id: string) {
    try {
      const inv = await invoke<{
        inventory: { identities: unknown[] };
        intended: { identities: unknown[] } | null;
      }>("collect_run_inventory", { runId: id, selectAll: true });
      if (!isActiveRun(id)) return;
      pushLog(
        `Inventory ${inv.inventory.identities.length} · intended ${inv.intended?.identities.length ?? 0}`,
      );
      await loadCaseReview(id);
    } catch {
      if (isActiveRun(id)) {
        pushLog("Inventory 跳过（非 Go 或失败）");
      }
    }
  }

  async function ensureRunProfile(targetRunId: string) {
    if (trusted) return;
    const suggestion = await invoke<{
      profile: Record<string, unknown>;
    }>("suggest_test_profile", { runId: targetRunId });
    await invoke("confirm_test_profile", {
      runId: targetRunId,
      profile: suggestion.profile,
    });
    if (isActiveRun(targetRunId)) {
      setTrusted(true);
      pushLog("已自动确认项目测试 Profile；高级配置仍可按需调整");
    }
  }

  async function onGenerate() {
    if (!runId) return;
    const operationRunId = runId;
    if (!availableAgentOptions.some((option) => option.value === agentMode)) {
      setSettingsMsg("请先在系统设置中启用一个已就绪的生成服务。");
      setSettingsOpen(true);
      return;
    }
    if (!agentReady) {
      setError(
        operationErrorMessage(
          selectedAgent?.error ??
            `${agentLabel} 尚未就绪，请完成配置后刷新状态。`,
        ),
      );
      return;
    }
    setError(null);
    setCaseDetail(null);
    setApprovedCase(false);
    setCanAcceptPass(false);
    setExecutionComplete(false);
    setExecution(null);
    setGenerationProgress({ stdout_tail: "", stderr_tail: "" });
    setGenerationRetryAvailable(false);
    setRunActivity(operationRunId, {
      kind: "generate",
      title: "Generating test candidate",
      detail: "Analyze code and generate test cases",
    });
    const normalizedObjective = objective.trim();
    setSidebarHistory((previous) =>
      previous.map((item) =>
        item.run_id === operationRunId
          ? {
              ...item,
              objective: normalizedObjective,
              updated_at: new Date().toISOString(),
            }
          : item,
      ),
    );
    const refreshProgress = async () => {
      try {
        const progress = await invoke<GenerationProgressDto>("generation_progress", {
          runId: operationRunId,
        });
        if (isActiveRun(operationRunId)) {
          setGenerationProgress(progress);
        }
      } catch {
        // 日志文件在生成任务完成初始化后才出现。
      }
    };
    let progressPolling = true;
    let progressTimer: number | undefined;
    const pollProgress = async () => {
      await refreshProgress();
      if (progressPolling) {
        progressTimer = window.setTimeout(() => void pollProgress(), 350);
      }
    };
    void pollProgress();
    try {
      const criteria = criteriaText
        .split("\n")
        .map((s) => s.trim())
        .filter(Boolean);
      const environment =
        testEnvironment.context.trim() ||
        testEnvironment.base_url.trim() ||
        testEnvironment.username.trim() ||
        testEnvironment.password ||
        Object.keys(testEnvironment.variables).length > 0
          ? { ...testEnvironment }
          : null;
      const sourceAnchors = sourceAnchorsText
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => {
          const separator = line.indexOf("=");
          return separator > 0
            ? { kind: line.slice(0, separator).trim(), value: line.slice(separator + 1).trim() }
            : { kind: "symbol", value: line };
        });
      const generationScope = generationScopeText
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean);
      const specAttachments = [
        ...(pastedSpecBody.trim()
          ? [{ kind: "pasted_text", label_or_path: pastedSpecLabel.trim() || "补充说明", body: pastedSpecBody }]
          : []),
        ...repoMarkdownText
          .split("\n")
          .map((line) => line.trim())
          .filter(Boolean)
          .map((path) => ({ kind: "repo_markdown", label_or_path: path, body: null })),
      ];
      const res = await invoke<{
        agent_summary: string;
        written_files: string[];
        review: {
          recommendation: string;
          findings: { id: string; disposition: string; message: string }[];
        };
        candidate: { candidate_tree: string; changed_paths: string[] };
      }>("generate_tests", {
        runId: operationRunId,
        objective: normalizedObjective,
        acceptanceCriteria: criteria,
        sourceAnchors,
        generationScope,
        specAttachments,
        testStrategy,
        cleanupTestData,
        agentMode,
        agentModel: agentMode !== "codex" && agentModel ? agentModel : null,
        reuseAgentSession,
        environment,
      });
      if (isActiveRun(operationRunId)) {
        pushLog(`[${agentLabel}] ${res.agent_summary}`);
        pushLog(
          `候选已冻结 · tree ${res.candidate.candidate_tree.slice(0, 10)}… · ${res.candidate.changed_paths.join(", ") || res.written_files.join(",")}`,
        );
        pushLog(
        `评审建议 ${reviewRecommendationLabel(res.review.recommendation)} · ${res.review.findings.length} 条发现`,
        );
      }
      if (isActiveRun(operationRunId)) {
        await loadCaseReview(operationRunId);
        await refreshEvents(operationRunId);
        void collectInventoryInBackground(operationRunId);
      }
    } catch (e) {
      if (isActiveRun(operationRunId)) {
        setError(operationErrorMessage(e));
        setGenerationRetryAvailable(true);
      }
    } finally {
      progressPolling = false;
      if (progressTimer !== undefined) window.clearTimeout(progressTimer);
      await refreshProgress();
      await refreshSidebarHistory();
      clearRunActivity(operationRunId);
    }
  }

  function onAgentModeChange(nextMode: string) {
    setAgentMode(nextMode);
    const allowed =
      nextMode === "grok"
        ? grokModelOptions
        : nextMode === "claude"
          ? claudeModelOptions
          : codexModelOptions;
    if (!allowed.some((option) => option.enabled && option.value === agentModel)) {
      setAgentModel("");
    }
  }

  async function onAgentPreferenceChange(mode: string, enabled: boolean) {
    const next = new Set(agentPreferences.enabled_modes);
    if (enabled) next.add(mode);
    else next.delete(mode);
    try {
      const preferences = await invoke<AgentPreferences>("set_agent_preferences", {
        enabledModes: Array.from(next),
      });
      setAgentPreferences(preferences);
      if (!enabled && agentMode === mode && !caseDetail) {
        setAgentMode(
          selectConfiguredAgentMode("", preferences, {
            codex: codex ?? { ready: false },
            claude: claude ?? { ready: false },
            grok: grok ?? { ready: false },
          }),
        );
      }
      setSettingsMsg("生成服务配置已保存。");
    } catch (e) {
      setError(operationErrorMessage(e));
    }
  }

  async function onLanguageChange(nextLanguage: Language) {
    const previousLanguage = language;
    setLanguage(nextLanguage);
    try {
      const preferences = await invoke<UiPreferences>("set_ui_language", {
        language: nextLanguage,
      });
      setLanguage(normalizeLanguage(preferences.language));
      setSettingsMsg(translate(nextLanguage, "Settings saved."));
    } catch (e) {
      setLanguage(previousLanguage);
      setError(operationErrorMessage(e));
    }
  }

  async function openCliInstallGuide(mode: string) {
    const url = cliInstallUrls[mode];
    if (!url) return;
    try {
      await openUrl(url);
    } catch (e) {
      setError(`无法打开安装指南：${String(e)}`);
    }
  }

  async function onAgentModelChange(nextModel: string) {
    setAgentModel(nextModel);
    if (!runId || (!caseDetail && !generationRetryAvailable)) return;
    setAgentModelSaving(true);
    try {
      await invoke("set_run_agent_model", {
        runId,
        agentModel: nextModel || null,
      });
    } catch (e) {
      setError(operationErrorMessage(e));
    } finally {
      setAgentModelSaving(false);
    }
  }

  async function onRetryGeneration(forceRegenerate = true) {
    if (!runId) return;
    const operationRunId = runId;
    setError(null);
    setApprovedCase(false);
    setExecution(null);
    setExecutionComplete(false);
    setCanAcceptPass(false);
    setGenerationProgress({ stdout_tail: "", stderr_tail: "" });
    setRunActivity(operationRunId, {
      kind: "generate",
      title: caseDetail?.pending_change_request
        ? "Generating a new revision from requested changes"
        : "Regenerating tests",
      detail:
        caseDetail?.pending_change_request ??
        "Continue from the saved test intent; the original error is retained",
    });
    const refreshProgress = async () => {
      try {
        const progress = await invoke<GenerationProgressDto>("generation_progress", {
          runId: operationRunId,
        });
        if (isActiveRun(operationRunId)) {
          setGenerationProgress(progress);
        }
      } catch {
        // Log files are created by the generation worker.
      }
    };
    let progressPolling = true;
    let progressTimer: number | undefined;
    const pollProgress = async () => {
      await refreshProgress();
      if (progressPolling) {
        progressTimer = window.setTimeout(() => void pollProgress(), 350);
      }
    };
    void pollProgress();
    try {
      const res = await invoke<{
        agent_summary: string;
        candidate: { candidate_tree: string; changed_paths: string[] };
        review: { recommendation: string; findings: unknown[] };
      }>("retry_test_generation", {
        runId: operationRunId,
        forceRegenerate,
      });
      if (isActiveRun(operationRunId)) {
        pushLog(`[${agentLabel}] ${res.agent_summary}`);
        pushLog(
          `新候选已冻结 · tree ${res.candidate.candidate_tree.slice(0, 10)}… · ${res.candidate.changed_paths.join(", ")}`,
        );
      }
      if (isActiveRun(operationRunId)) {
        setGenerationRetryAvailable(false);
        await loadCaseReview(operationRunId);
        await refreshEvents(operationRunId);
        void collectInventoryInBackground(operationRunId);
      }
    } catch (e) {
      if (isActiveRun(operationRunId)) {
        setGenerationRetryAvailable(true);
        setError(operationErrorMessage(e));
      }
    } finally {
      progressPolling = false;
      if (progressTimer !== undefined) window.clearTimeout(progressTimer);
      await refreshProgress();
      await refreshSidebarHistory();
      clearRunActivity(operationRunId);
    }
  }

  async function onSetDisposition(findingId: string, disposition: string) {
    if (!runId) return;
    setError(null);
    try {
      let riskReason: string | null = null;
      if (disposition === "accepted_risk") {
        riskReason = (riskDrafts[findingId] || "").trim() || "reviewed in UI";
      }
      await invoke("set_finding", {
        runId,
        findingId,
        disposition,
        riskReason,
      });
      await loadCaseReview(runId);
    } catch (e) {
      setError(String(e));
    }
  }

  async function onApproveCase() {
    if (!runId) return;
    const operationRunId = runId;
    setBusy(true);
    setError(null);
    setActivity({
      kind: "review",
      title: "Approving test case",
      detail: "Bind the current candidate and review snapshot",
    });
    try {
      // Inventory is part of the approval binding. Finish it before approval;
      // collecting it after approval would change the approved manifest hash.
      await collectInventoryInBackground(operationRunId);
      const r = await invoke<{ workflow_state: string }>("approve_case", {
        runId: operationRunId,
      });
      setApprovedCase(true);
      pushLog(`用例已批准 → ${r.workflow_state}`);
      void loadCaseReview(operationRunId);
      void refreshEvents(operationRunId);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onCommentCase() {
    if (!runId || !changeRequest.trim()) {
      setError("请填写评论内容");
      return;
    }
    const operationRunId = runId;
    const message = changeRequest.trim();
    setBusy(true);
    setError(null);
    setActivity({
      kind: "review",
      title: "Adding review comment",
      detail: "Comments are added to the audit log without changing workflow state",
    });
    try {
      const r = await invoke<{ workflow_state: string }>("comment_case", {
        runId: operationRunId,
        message,
      });
      pushLog(`已添加评审评论 · ${r.workflow_state}`);
      setChangeRequest("");
      await loadCaseReview(operationRunId);
      await refreshEvents(operationRunId);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onRequestChanges() {
    if (!runId || !changeRequest.trim()) {
      setError("请填写变更请求说明");
      return;
    }
    setBusy(true);
    setError(null);
    setActivity({
      kind: "review",
      title: "Recording change request",
      detail: "The current approval is invalidated and the task returns to generation",
    });
    try {
      await invoke("request_changes", {
        runId,
        message: changeRequest.trim(),
      });
      pushLog(`已记录变更请求: ${changeRequest.trim()}`);
      setChangeRequest("");
      setApprovedCase(false);
      await loadCaseReview(runId);
      await refreshEvents(runId);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onRejectCase() {
    if (!runId || !changeRequest.trim()) {
      setError("请填写拒绝原因");
      return;
    }
    const operationRunId = runId;
    const message = changeRequest.trim();
    const ok = window.confirm(
      `拒绝当前测试用例？\n\n原因：${message}\n\n该任务会进入 Rejected 终态，不能再直接执行。`,
    );
    if (!ok) return;
    setBusy(true);
    setError(null);
    setActivity({
      kind: "review",
      title: "Rejecting test case",
      detail: "Record a human rejection decision and end the current task",
    });
    try {
      const r = await invoke<{ workflow_state: string }>("reject_case", {
        runId: operationRunId,
        message,
      });
      pushLog(`用例已拒绝 → ${r.workflow_state}`);
      setChangeRequest("");
      setApprovedCase(false);
      setExecution(null);
      setExecutionComplete(false);
      setCanAcceptPass(false);
      await loadCaseReview(operationRunId);
      await refreshEvents(operationRunId);
      await refreshSidebarHistory();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  function applyCompletedExecution(p: ExecutionPollDto) {
    setExecution(p);
    setExecutionComplete(true);
    if (p.generic_result) pushLog(p.generic_result.summary);
    if (p.structured) {
      pushLog(
        `结构化结果 total=${p.structured.total} · pass=${p.structured.passed} · fail=${p.structured.failed}`,
      );
    }
    if (p.accept_pass) {
      setCanAcceptPass(p.accept_pass.enabled);
      setAcceptInfo(
        p.accept_pass.enabled
          ? "结果证据完整，可以接受通过"
          : `暂不能接受通过：${p.accept_pass.reasons.slice(0, 2).join("；")}`,
      );
    } else {
      setCanAcceptPass(false);
    }
    if (p.analysis) {
      const top = p.analysis.findings
        .slice(0, 3)
        .map((f) => `[${f.severity}] ${f.message}`)
        .join(" · ");
      setAnalysisInfo(
        `${p.analysis.suggested_verdict} (${p.analysis.confidence}) — ${p.analysis.summary}${top ? ` · ${top}` : ""}`,
      );
      pushLog(`结果分析：${p.analysis.suggested_verdict} · ${p.analysis.summary}`);
    }
    if (p.result_review) {
      const top = p.result_review.findings
        .slice(0, 3)
        .map((f) => `[${f.severity}] ${f.message}`)
        .join(" · ");
      setResultReviewInfo(
        `${reviewRecommendationLabel(p.result_review.recommendation)}${p.result_review.analyzer_overreach ? " · 结论可能超出证据" : ""} — ${p.result_review.summary}${top ? ` · ${top}` : ""}`,
      );
      pushLog(`结果复核：${reviewRecommendationLabel(p.result_review.recommendation)}`);
    }
    if (p.agent_error) {
      setError(operationErrorMessage(p.agent_error));
      pushLog(`智能分析失败：${p.agent_error}`);
    }
  }

  async function onTrustAndRun(targetRunId?: string, caseId?: string) {
    const operationRunId = targetRunId ?? runId;
    if (!operationRunId) return;
    if (isActiveRun(operationRunId)) {
      setError(null);
      setExecution(null);
      setExecutionComplete(false);
      setCanAcceptPass(false);
      setAcceptInfo(null);
    }
    cancelRequestedRuns.current.delete(operationRunId);
    setRunActivity(operationRunId, {
      kind: "execute",
      title: "Preparing test environment",
      detail: "Confirm command permissions and start the test process",
    });
    try {
      await ensureRunProfile(operationRunId);
      const started = await invoke<{ outcome: ProcessOutcomeDto; ui_warning: string }>(
        "start_execution",
        { runId: operationRunId, caseId: caseId ?? null },
      );
      if (isActiveRun(operationRunId)) {
        setExecution({
          outcome: started.outcome,
          stdout_tail: "",
          stderr_tail: "",
          generic_result: null,
          accept_pass: null,
          structured: null,
          analysis: null,
          result_review: null,
          agent_error: null,
        });
        if (started.ui_warning) pushLog(started.ui_warning);
        setAnalysisInfo(null);
        setResultReviewInfo(null);
      }
      setRunActivity(operationRunId, {
        kind: "execute",
        title: "Executing tests",
        detail: "Live output appears below; long tasks use the configured test timeout",
      });
      while (!cancelRequestedRuns.current.has(operationRunId)) {
        let slowPollNotice: number | null = null;
        if (isActiveRun(operationRunId)) {
          slowPollNotice = window.setTimeout(() => {
            if (!cancelRequestedRuns.current.has(operationRunId) && isActiveRun(operationRunId)) {
              setRunActivity(operationRunId, {
                kind: "execute",
                title: "Waiting for test-result analysis",
                detail: "The test process may have exited; parsing structured results and running independent AI result review",
              });
            }
          }, 6000);
        }
        let p: ExecutionPollDto;
        try {
          p = await invoke<ExecutionPollDto>("poll_run_execution", {
            runId: operationRunId,
          });
        } finally {
          if (slowPollNotice != null) window.clearTimeout(slowPollNotice);
        }
        if (isActiveRun(operationRunId)) {
          setExecution(p);
          if (!p.generic_result) {
            setRunActivity(operationRunId, {
              kind: "execute",
              title: "Executing tests",
              detail: "Live output appears below; long tasks use the configured test timeout",
            });
          }
        }
        if (p.generic_result) {
          if (isActiveRun(operationRunId)) {
            applyCompletedExecution(p);
            await refreshEvents(operationRunId);
          }
          break;
        }
        await new Promise((r) => setTimeout(r, 500));
      }
    } catch (e) {
      if (!cancelRequestedRuns.current.has(operationRunId) && isActiveRun(operationRunId)) {
        const message = operationErrorMessage(e);
        setError(message);
        if (message.includes("批准后的测试清单发生变化")) {
          setApprovedCase(false);
          setAcceptInfo("批准绑定已失效，请重新批准当前用例后再执行。");
          await loadCaseReview(operationRunId);
          await refreshEvents(operationRunId);
        } else if (message.includes("CandidateTreeMismatch") && message.includes("已回到用例评审")) {
          setApprovedCase(false);
          setAcceptInfo("批准后的测试源码已变化，请重新确认候选后再执行。");
          await loadCaseReview(operationRunId);
          await refreshEvents(operationRunId);
        }
      }
    } finally {
      await refreshSidebarHistory();
      cancelRequestedRuns.current.delete(operationRunId);
      clearRunActivity(operationRunId);
    }
  }

  async function onCancelExecution() {
    if (!runId || currentRunActivity?.kind !== "execute") return;
    const operationRunId = runId;
    cancelRequestedRuns.current.add(operationRunId);
    setRunActivity(operationRunId, {
      kind: "execute",
      title: "Stopping test",
      detail: "Terminate the test process group",
    });
    try {
      const p = await invoke<ExecutionPollDto>("cancel_run_execution", {
        runId: operationRunId,
      });
      if (isActiveRun(operationRunId)) {
        applyCompletedExecution(p);
        setAcceptInfo("测试已取消。当前结果不完整，未应用任何修改。");
        pushLog("测试执行已取消");
      }
    } catch (e) {
      if (isActiveRun(operationRunId)) {
        setError(String(e));
      }
    } finally {
      await refreshSidebarHistory();
      cancelRequestedRuns.current.delete(operationRunId);
      clearRunActivity(operationRunId);
    }
  }

  async function onCancelGeneration() {
    if (!runId || currentRunActivity?.kind !== "generate") return;
    const operationRunId = runId;
    setRunActivity(operationRunId, {
      kind: "generate",
      title: "Terminating generation",
      detail: "Terminate the Codex process and retain output for this run",
    });
    try {
      await invoke<boolean>("cancel_test_generation", { runId: operationRunId });
    } catch (e) {
      if (isActiveRun(operationRunId)) setError(operationErrorMessage(e));
    }
  }

  async function onAcceptPass() {
    if (!runId) return;
    setBusy(true);
    setError(null);
    setActivity({
      kind: "delivery",
      title: "Accepting formal execution result",
      detail: "Recheck completeness, intended observation, and independent Agent result review",
    });
    try {
      const d = await invoke<{ message: string }>("result_accept_pass", {
        runId,
      });
      pushLog(d.message);
      setReadyToApply(true);
      setAcceptInfo("结果已接受，Run 进入 ReadyToApply；交付仍需单独确认。");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onApplyAcceptedRevision() {
    if (!runId) return;
    setBusy(true);
    setError(null);
    setActivity({
      kind: "delivery",
      title: "Checking and applying changes",
      detail: "Recheck repository clean state, HEAD, scope, and candidate tree",
    });
    try {
      const g = await invoke<{
        enabled: boolean;
        reasons: string[];
        head_drift: boolean;
        successor_hint: string | null;
      }>("get_apply_guard", { runId });
      if (!g.enabled) {
        setAcceptInfo(
          g.head_drift
            ? "HEAD 已漂移，不能 Apply。请导出 Patch 或创建 Successor Run。"
            : `Apply 阻塞: ${g.reasons.join("; ")}`,
        );
        return;
      }
      const a = await invoke<{
        applied: { applied_commit: string };
        next_steps: string[];
      }>("apply_run_patch", { runId });
      pushLog(`Applied ${a.applied.applied_commit.slice(0, 12)}…`);
      setAcceptInfo(`已 Apply · ${a.next_steps[0] ?? ""}`);
      setReadyToApply(false);
      await refreshHistory();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onCreateSuccessor() {
    if (!runId) return;
    setBusy(true);
    setError(null);
    setActivity({
      kind: "delivery",
      title: "Continuing from latest code",
      detail: "Replay the candidate onto the new HEAD and re-establish review guards",
    });
    try {
      const r = await invoke<{
        successor_run_id: string;
        new_base_commit: string;
        old_base_commit: string;
        message: string;
        next_steps: string[];
        candidate: { changed_paths: string[] };
      }>("create_run_successor", { runId });
      pushLog(r.message);
      pushLog(
        `Successor ${r.successor_run_id.slice(0, 8)}… base ${r.old_base_commit.slice(0, 7)}→${r.new_base_commit.slice(0, 7)} · files ${r.candidate.changed_paths.join(",")}`,
      );
      for (const s of r.next_steps.slice(0, 3)) pushLog(`→ ${s}`);
      setRunId(r.successor_run_id);
      activeRunIdRef.current = r.successor_run_id;
      window.localStorage.setItem("testharbor:last-run", r.successor_run_id);
      setApprovedCase(false);
      setTrusted(false);
      setAcceptInfo(null);
      setAnalysisInfo(null);
      setResultReviewInfo(null);
      setExecution(null);
      setExecutionComplete(false);
      setCanAcceptPass(false);
      setCaseDetail(null);
      setAcceptInfo("已切换到 Successor Run：请重新批准用例并执行");
      await loadCaseReview(r.successor_run_id);
      await refreshEvents(r.successor_run_id);
      await refreshHistory();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onExport() {
    if (!runId) return;
    try {
      const r = await invoke<{ path: string | null }>("export_run_patch", {
        runId,
        complete: false,
      });
      pushLog(`导出: ${r.path ?? "ok"}`);
    } catch (e) {
      setError(String(e));
    }
  }

  async function onResultDecision(
    command:
      | "result_accept_fail"
      | "result_end_no_decision"
      | "result_mark_bug"
      | "result_mark_env"
      | "result_mark_flaky",
  ) {
    if (!runId) return;
    setBusy(true);
    setError(null);
    const decisionCopy = {
      result_accept_fail: ["Recording test failure", "Save execution evidence and the human conclusion without modifying the main workspace"],
      result_end_no_decision: ["Ending task", "Retain current evidence without deciding pass or fail"],
      result_mark_bug: ["Marking product defect", "Save failure evidence and archive as a product issue"],
      result_mark_env: ["Marking environment issue", "Retain evidence and rerun after the environment is fixed"],
      result_mark_flaky: ["Marking flaky test", "Save current evidence and archive as Flaky"],
    } as const;
    setActivity({
      kind: "delivery",
      title: decisionCopy[command][0],
      detail: decisionCopy[command][1],
    });
    try {
      const result = await invoke<{ message: string }>(command, { runId });
      pushLog(result.message);
      await refreshHistory();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onRerunExecution(targetRunId?: string, caseId?: string) {
    const operationRunId = targetRunId ?? runId;
    if (!operationRunId) return;
    setError(null);
    setRunActivity(operationRunId, {
      kind: "execute",
      title: "Preparing rerun",
      detail: "Reuse the approved test candidate, reconfirm commands, and execute",
    });
    try {
      const rerun = await invoke<{ decision: string; message: string }>("result_rerun", {
        runId: operationRunId,
      });
      if (isActiveRun(operationRunId)) {
        setExecution(null);
        setExecutionComplete(false);
        setCanAcceptPass(false);
        setAcceptInfo(null);
      }
      if (rerun.decision === "reapprove") {
        if (isActiveRun(operationRunId)) {
          setApprovedCase(false);
          setAcceptInfo(rerun.message);
          pushLog(rerun.message);
          await loadCaseReview(operationRunId);
          await refreshEvents(operationRunId);
        }
        return;
      }
      await onTrustAndRun(operationRunId, caseId);
    } catch (e) {
      if (isActiveRun(operationRunId)) {
        setError(String(e));
      }
    } finally {
      await refreshSidebarHistory();
      clearRunActivity(operationRunId);
    }
  }

  async function onRequestResultTestChange() {
    if (!runId || !changeRequest.trim()) return;
    setBusy(true);
    setError(null);
    setActivity({
      kind: "generate",
      title: "Creating a new Revision from result feedback",
      detail: "Old Revisions, Executions, and decisions remain in history; downstream pointers are invalidated",
      run_id: runId,
    });
    try {
      await invoke("result_request_test_change", {
        runId,
        feedback: changeRequest.trim(),
      });
      setChangeRequest("");
      setExecution(null);
      setExecutionComplete(false);
      setCanAcceptPass(false);
      setReadyToApply(false);
      setApprovedCase(false);
      await loadCaseReview(runId);
      await refreshEvents(runId);
      await refreshHistory();
    } catch (error) {
      setError(operationErrorMessage(error));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onCleanup() {
    setBusy(true);
    setActivity({
      kind: "maintenance",
      title: "Cleaning local data",
      detail: "Clear completed build caches and terminal worktrees while retaining the latest 20 completed records",
    });
    try {
      const r = await invoke<{
        messages: string[];
        freed_bytes_estimate: number;
        deleted_runs: string[];
        removed_worktrees: string[];
        removed_cache_dirs: string[];
      }>("run_cleanup", {
        removeTerminalWorktrees: true,
        deleteCompletedBeyond: 20,
      });
      setSettingsMsg(
        `${r.messages.join(" · ")} · 约释放 ${fmtBytes(r.freed_bytes_estimate)} · 删 Run ${r.deleted_runs.length} · worktree ${r.removed_worktrees.length} · 构建缓存 ${r.removed_cache_dirs.length}`,
      );
      setUsage(await invoke<DiskUsage>("get_disk_usage"));
      await refreshHistory();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setActivity(null);
    }
  }

  async function onRecover() {
    try {
      const r = await invoke<RecoveryReport>("run_recovery");
      setSettingsMsg(
        `${r.messages.join(" ")} · lost/needs_human=${r.marked_lost.length}`,
      );
      await refreshHistory();
    } catch (e) {
      setError(String(e));
    }
  }

  const activeRunHistory =
    sidebarHistory.find((item) => item.run_id === runId) ?? null;
  const projectSupportsCaseRerun = Boolean(
    projectReadiness?.profiles.some((profile) => profile.parser === "go-test-json"),
  );
  const showGenerationWorkspace =
    currentRunActivity?.kind === "generate" ||
    activeRunHistory?.workflow_state === "GeneratingTests" ||
    (!caseDetail && generationRetryAvailable);
  const generationIsLive = currentRunActivity?.kind === "generate";
  const testRows = parseTestRows(execution?.stdout_tail ?? "");
  const executionProcessComplete = Boolean(
    execution &&
      ["exited", "timed_out", "cancelled", "failed"].includes(execution.outcome.phase),
  );
  const executionProcessFailed =
    executionProcessComplete &&
    (execution?.outcome.phase !== "exited" ||
      (execution.outcome.exit_code ?? 1) !== 0 ||
      (execution.structured?.failed ?? 0) > 0);
  const executionFailed =
    executionComplete &&
    (execution?.outcome.phase !== "exited" ||
      (execution.outcome.exit_code ?? 1) !== 0 ||
      (execution.structured?.failed ?? 0) > 0);

  const auditPanel = runId && (log.length > 0 || events.length > 0) && (
    <details className="audit-drawer">
      <summary>
        <span>{t("Run dossier")}</span>
        <span>{t("{count} records", { count: log.length + events.length })}</span>
      </summary>
      <div className="audit-columns">
        <div>
          <p className="section-kicker">{t("Activity log")}</p>
          <ul className="audit-list">
            {log.map((line, index) => (
              <li key={`${line}-${index}`}>
                <code>{line}</code>
              </li>
            ))}
          </ul>
        </div>
        <div>
          <p className="section-kicker">{t("Audit events")}</p>
          <ul className="audit-list">
            {events.map((event, index) => (
              <li key={`${event.event_type}-${index}`}>
                <code>{event.event_type}</code>
                <time>{event.created_at}</time>
              </li>
            ))}
            {events.length === 0 && <li className="empty-line">{t("No audit events")}</li>}
          </ul>
        </div>
      </div>
    </details>
  );

  return (
    <div className={`app-shell ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}>
      <a className="skip-link" href="#main-content">
        {t("Skip to main content")}
      </a>

      <WorkspaceSidebar
        projects={recent}
        runs={sidebarHistory.map((item) => {
          const running = runActivities[item.run_id];
          if (!running) return item;
          return {
            ...item,
            workflow_state:
              running.kind === "execute" ? "ExecutingTests" : "GeneratingTests",
            status_bucket: "active",
          };
        })}
        activeProjectId={project?.id ?? null}
        activeRunId={runId}
        busy={operationBusy}
        onSelectProject={(item) => {
          const selected = recent.find((projectItem) => projectItem.id === item.id);
          if (selected) void selectProject(selected);
        }}
        onSelectRun={(item) => {
          const selected = sidebarHistory.find(
            (historyItem) => historyItem.run_id === item.run_id,
          );
          if (selected) void openHistoryRun(selected);
        }}
        onCreateRun={(item) => {
          const selected = item
            ? recent.find((projectItem) => projectItem.id === item.id) ?? null
            : null;
          void createRunForProject(selected);
        }}
        onDeleteRun={(item) => {
          const selected = sidebarHistory.find(
            (historyItem) => historyItem.run_id === item.run_id,
          );
          if (selected) void deleteHistoryRun(selected);
        }}
        onAddProject={() => {
          setPathInput("");
          void pickAndOpen(false);
        }}
        onOpenSettings={() => setSettingsOpen(true)}
        serviceReady={agentReady}
        sidebarCollapsed={sidebarCollapsed}
        onToggleSidebar={toggleSidebar}
        language={language}
      />

      <div className="app-frame">
        <header className="workbench-topbar">
          <div className="workbench-breadcrumb">
            <button
              type="button"
              onClick={() => setTab("project")}
              title={project?.root_path ?? t("Project workspace")}
            >
              <span className="breadcrumb-folder" aria-hidden="true">⌁</span>
              <strong>{project?.name ?? t("Select a project")}</strong>
            </button>
            {tab === "run" && runId && (
              <>
                <span className="breadcrumb-separator" aria-hidden="true">/</span>
                <span className="breadcrumb-task">
                  {caseDetail?.objective || objective || t("New test task")}
                </span>
              </>
            )}
          </div>
          <div className="workbench-actions">
            {repo?.branch && (
              <span className="workspace-meta" title={t("Current branch {branch}", { branch: repo.branch })}>
                <span aria-hidden="true">⑂</span>
                {repo.branch}
              </span>
            )}
            {repo && (
              <code className="workspace-base" title={t("Task baseline commit")}>
                {repo.short_commit}
              </code>
            )}
          </div>
        </header>

        <main className="app-main" id="main-content">
          {recoveryBanner && (
            <div className="notice-strip" role="status">
              <span>{t("Recovery check")}</span>
              <p>{recoveryBanner}</p>
              <button type="button" onClick={() => setRecoveryBanner(null)}>
                {t("Got it")}
              </button>
            </div>
          )}
          {error && (
            <div className="error-strip" role="alert">
              <span>{t("Operation incomplete")}</span>
              <p>{error}</p>
              <button type="button" onClick={() => setError(null)}>
                {t("Close")}
              </button>
            </div>
          )}
        {activity && (
          <div className="activity-strip" aria-live="polite">
            <span className="activity-pulse" />
            <div>
              <strong>{t(activity.title)}</strong>
              <p>
                {!globalActivity && backgroundActivity
                  ? t("{count} background tasks are still running · ", { count: backgroundActivities.length })
                  : ""}
                {t(activity.detail)}
              </p>
            </div>
          </div>
        )}

        {tab === "run" &&
          runId &&
          (caseDetail || generationRetryAvailable) &&
          configuredAgentMode !== "mock" && (
          <div className="agent-context-bar">
            <div>
              <span className="section-kicker">NEXT AGENT STEP</span>
              <strong>{t("The model used for subsequent {service} steps", { service: agentLabel })}</strong>
            </div>
            <AgentSelect
              value={agentModel}
              options={agentModelOptions}
              disabled={busy || agentModelSaving}
              onChange={(value) => void onAgentModelChange(value)}
            />
          </div>
        )}

        {tab === "project" && (
          <div className="page-stack">
            <header className="page-title-row">
              <div>
                <p className="section-kicker">PROJECT WORKSPACE / 01</p>
                <h1>{project?.name ?? t("Open a local project")}</h1>
                <p>
                  {project
                    ? t("The project stays in the sidebar. Continue an existing task or create one from the current HEAD.")
                    : t("Add a local Git repository and it will stay pinned here.")}
                </p>
              </div>
              <StateMark
                tone={
                  projectReadiness
                    ? capabilityTone(projectReadiness.capability)
                    : repo
                      ? repo.is_dirty
                        ? "warning"
                        : "success"
                      : "neutral"
                }
                label={projectReadiness ? t(projectReadiness.capability_label) : (repo ? t("Repository ready") : t("Waiting for repository"))}
              />
            </header>

            <div className="project-workspace">
              <section className="workspace-panel project-intake">
                <div className="panel-heading">
                  <div>
                    <p className="section-kicker">{t("Repository overview")}</p>
                    <h2>{project ? t("Current project ready") : t("Add your first project")}</h2>
                  </div>
                  <span className="panel-index">01</span>
                </div>

                {repo ? (
                  <div className="repo-readiness">
                    <div className="repo-identity">
                      <span className="repo-glyph" aria-hidden="true">
                        GIT
                      </span>
                      <div>
                        <strong>{project?.name ?? repo.root.split("/").pop()}</strong>
                        <code>{repo.root}</code>
                      </div>
                    </div>
                    <dl className="definition-list">
                      <div>
                        <dt>{t("Current branch")}</dt>
                        <dd>{repo.branch ?? "detached HEAD"}</dd>
                      </div>
                      <div>
                        <dt>{t("Baseline commit")}</dt>
                        <dd>
                          <code>{repo.short_commit}</code>
                        </dd>
                      </div>
                      <div>
                        <dt>{t("Workspace status")}</dt>
                        <dd>{repo.is_dirty ? t("Local changes will be isolated from the run baseline") : t("Clean")}</dd>
                      </div>
                      <div>
                        <dt>{t("Detected languages")}</dt>
                        <dd>
                          {projectReadiness?.detected_stacks.length
                            ? projectReadiness.detected_stacks.join(" / ")
                            : t("Unrecognized")}
                        </dd>
                      </div>
                      <div>
                        <dt>{t("Test files")}</dt>
                        <dd>{projectReadiness?.test_file_count ?? 0}</dd>
                      </div>
                      <div>
                        <dt>{t("Apply policy")}</dt>
                        <dd>{t(repo.apply_hint)}</dd>
                      </div>
                    </dl>
                    {projectReadiness && (
                      <>
                        <div className="project-capability-band">
                          <StateMark
                            tone={capabilityTone(projectReadiness.capability)}
                            label={projectReadiness.capability_label}
                          />
                          <p>{t(projectReadiness.summary)}</p>
                        </div>
                        <div className="profile-list">
                          {projectReadiness.profiles.map((profile) => (
                            <div
                              key={profile.key}
                              className={`profile-card ${profile.recommended ? "recommended" : ""}`}
                            >
                              <div className="profile-card-heading">
                                <strong>{t(profile.label)}</strong>
                                <StateMark
                                  tone={
                                    profile.available
                                      ? profile.strict || profile.structured
                                        ? "success"
                                        : "warning"
                                      : "danger"
                                  }
                                  label={
                                    profile.available
                                      ? profile.strict
                                        ? t("Strict structured")
                                        : profile.structured
                                          ? t("Structured")
                                          : t("Command-level")
                                      : t("Environment missing")
                                  }
                                />
                              </div>
                              <code>{profile.command_preview}</code>
                              <p>
                                parser={profile.parser}
                                {profile.structured && !profile.strict
                                  ? ` · ${t("Supports structured result review")}`
                                  : ""}
                                {profile.recommended ? ` · ${t("Recommended")}` : ""}
                              </p>
                            </div>
                          ))}
                        </div>
                        <div className="readiness-checks detailed">
                          {projectReadiness.checks.map((check) => (
                            <div key={check.key} className="readiness-check-card">
                              <div className="readiness-check-head">
                                <strong>{check.label}</strong>
                                <StateMark
                                  tone={readinessTone(check.status)}
                                  label={readinessStatusLabel(check.status)}
                                />
                              </div>
                              <p>{t(check.detail)}</p>
                            </div>
                          ))}
                          <div className="readiness-check-card">
                            <div className="readiness-check-head">
                              <strong>{t("Git / Apply constraints")}</strong>
                              <StateMark
                                tone={repo.is_dirty ? "warning" : "success"}
                                label={repo.is_dirty ? "working tree dirty" : "head bindable"}
                              />
                            </div>
                            <p>{t(repo.apply_hint)}</p>
                          </div>
                        </div>
                      </>
                    )}
                  </div>
                ) : (
                  <div className="project-empty">
                    <span className="project-empty-mark" aria-hidden="true">
                      ↳
                    </span>
                    <h3>{project ? t("Reading project status") : t("Projects remain available here")}</h3>
                    <p>
                      {project
                        ? project.root_path
                        : t("Choose a local Git repository once; it remains available from the sidebar.")}
                    </p>
                    {!project && (
                      <button
                        type="button"
                        className="primary-action project-empty-action"
                        disabled={busy}
                        onClick={() => {
                          setPathInput("");
                          void pickAndOpen(false);
                        }}
                      >
                        {t("Open local repository")} <span>→</span>
                      </button>
                    )}
                  </div>
                )}

                <div className="primary-action-row">
                  <p>
                    {project
                      ? t("Creating a task locks the current HEAD and performs generation, review, and execution in an isolated worktree.")
                      : t("Projects appear in the sidebar and their tasks are grouped automatically.")}
                  </p>
                  <button
                    type="button"
                    className="primary-action"
                    disabled={busy || !project}
                    onClick={() => void createRunForProject(project)}
                  >
                    {t("Create a new task in the current project")} <span>→</span>
                  </button>
                </div>
              </section>

              <aside className="dossier">
                <div className="dossier-heading">
                  <p className="section-kicker">{t("Connection profile")}</p>
                  <span>LOCAL ONLY</span>
                </div>
                <dl className="definition-list compact">
                  <div>
                    <dt>{t("Product version")}</dt>
                    <dd>{info?.version ?? "—"}</dd>
                  </div>
                  <div>
                    <dt>{t("Generation service")}</dt>
                    <dd>{agentReady ? t("{service} ready", { service: agentLabel }) : t("{service} requires configuration", { service: agentLabel })}</dd>
                  </div>
                  <div>
                    <dt>{t("Data location")}</dt>
                    <dd className="path-value">{info?.data_dir ?? t("Application data directory")}</dd>
                  </div>
                </dl>
                {project && (
                  <button
                    type="button"
                    className="dossier-secondary-action"
                    disabled={busy}
                    onClick={() => {
                      setPathInput("");
                      void pickAndOpen(false);
                    }}
                  >
                    {t("Add another project")}
                  </button>
                )}
              </aside>
            </div>

          <section className="workspace-panel project-case-library">
            <div className="panel-heading">
              <div>
                <p className="section-kicker">PROJECT CASE LIBRARY</p>
                <h2>{t("Project case library")}</h2>
                <p className="panel-subtitle">
                  {t("Business cases generated by project tasks are collected here; reruns reuse the approved candidate and isolated environment.")}
                </p>
              </div>
              <span className="panel-index">{projectCases.length}</span>
            </div>
            {projectCasesLoading ? (
              <p className="project-case-library-empty">{t("Loading project cases…")}</p>
            ) : projectCases.length > 0 ? (
              <div className="project-case-grid">
                {projectCases.map((item) => (
                  <article className="project-case-card" key={item.key}>
                    <div className="project-case-card-head">
                      <span className="project-case-number">CASE</span>
                      <StateMark
                        tone={item.can_rerun && projectSupportsCaseRerun ? "success" : item.approved ? "warning" : "neutral"}
                        label={item.can_rerun && projectSupportsCaseRerun ? t("Rerunnable") : item.approved ? t("Recovery required") : t("Not approved")}
                      />
                    </div>
                    <h3>{item.label}</h3>
                    <p>{item.objective || t("No scenario objective")}</p>
                    <div className="project-case-card-meta">
                      <span>{t("{count} business steps", { count: item.step_count })}</span>
                      <span>{item.run.objective || t("Unnamed task")}</span>
                    </div>
                    <div className="project-case-card-actions">
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => void openHistoryRun(item.run)}
                      >
                        {t("View case")}
                      </button>
                      <button
                        type="button"
                        className="project-case-rerun"
                        disabled={busy || !item.can_rerun || !projectSupportsCaseRerun}
                        title={
                          item.can_rerun && projectSupportsCaseRerun
                            ? t("Run only the independent Go test entry for this business case")
                            : t("Requires the Go Test JSON driver, an approved candidate, and an executable task state")
                        }
                        onClick={() =>
                          void (async () => {
                            await openHistoryRun(item.run);
                            await onRerunExecution(item.run.run_id, item.case_id);
                          })()
                        }
                      >
                        {t("Rerun this case")}
                      </button>
                    </div>
                  </article>
                ))}
              </div>
            ) : (
              <div className="project-case-library-empty">
                <strong>{t("No project cases yet")}</strong>
                <p>{t("Cases with business-step contracts appear here after a test candidate is generated.")}</p>
              </div>
            )}
          </section>
          </div>
        )}

        {tab === "run" &&
          (!runId ? (
            <div className="empty-workspace">
              <p className="section-kicker">NO ACTIVE RUN</p>
              <h1>{t("No test task selected")}</h1>
              <p>{t("Choose an existing task from the sidebar or create a new one in this project.")}</p>
              <button
                type="button"
                className="primary-action"
                disabled={!project}
                onClick={() => void createRunForProject(project)}
              >
                {t("Create a new task in the current project")} <span>→</span>
              </button>
            </div>
          ) : (
            <div className="run-page">
              {currentRunActivity && (
                <div className="activity-strip run-activity" aria-live="polite">
                  <span className="activity-pulse" />
                  <div>
                    <strong>{currentRunActivity.title}</strong>
                    <p>{currentRunActivity.detail}</p>
                  </div>
                  {currentRunActivity.kind === "execute" && (
                    <button
                      type="button"
                      className="danger-action"
                      disabled={currentRunActivity.title === "Stopping test"}
                      onClick={onCancelExecution}
                    >
                      {currentRunActivity.title === "Stopping test" ? t("Stopping…") : t("Stop test")}
                    </button>
                  )}
                  {currentRunActivity.kind === "generate" && (
                    <button
                      type="button"
                      className="danger-action"
                      disabled={currentRunActivity.title === "Terminating generation"}
                      onClick={onCancelGeneration}
                    >
                      {currentRunActivity.title === "Terminating generation" ? t("Terminating…") : t("Terminate generation")}
                    </button>
                  )}
                </div>
              )}

              {showGenerationWorkspace ? (
                <div className="two-column-workspace">
                  <section className="workspace-panel progress-workspace">
                    <div className="panel-heading">
                      <div>
                        <p className="section-kicker">GENERATION / LIVE</p>
                        <h2>{generationIsLive ? t("Building test candidate") : t("Generation incomplete")}</h2>
                      </div>
                      <StateMark
                        tone={generationIsLive ? "active" : "warning"}
                        label={generationIsLive ? t("Generating") : t("Recoverable")}
                      />
                    </div>
                    {generationIsLive && (
                      <div className="progress-track" aria-hidden="true">
                        <span />
                      </div>
                    )}
                    <ol className="operation-list">
                      <li className="complete">
                        <span>01</span>
                        <div>
                          <strong>{t("Lock baseline and isolated directory")}</strong>
                          <p>{t("The version baseline for this task is established.")}</p>
                        </div>
                      </li>
                      <li className="current">
                        <span>02</span>
                        <div>
                          <strong>{t("Analyze objective and generate tests")}</strong>
                          <p>
                            {generationIsLive
                              ? currentRunActivity?.detail
                              : t("The previous generation did not finish. Continue with the saved test intent.")}
                          </p>
                        </div>
                      </li>
                      <li>
                        <span>03</span>
                        <div>
                          <strong>{t("Freeze candidate and run static review")}</strong>
                          <p>{t("Opens the three-column Diff workbench when complete.")}</p>
                        </div>
                      </li>
                    </ol>
                    <div className="live-log">
                      <div>
                        <span>{t("Live output")}</span>
                        <span>{generationIsLive ? "RUNNING" : "PAUSED"}</span>
                      </div>
                      <pre ref={generationConsoleRef} aria-live="polite">
                        {generationProgress.stdout_tail.trim()
                          ? formatCodexConsole(generationProgress.stdout_tail)
                          : generationIsLive
                            ? t("Starting generation service and waiting for CLI output…")
                            : t("No recoverable generation output.")}
                      </pre>
                      {generationProgress.stderr_tail.trim() && (
                        <>
                          <div className="live-log-channel">
                            <span>{t("Diagnostics")}</span>
                            <span>STDERR</span>
                          </div>
                          <pre className="live-log-errors">
                            {generationProgress.stderr_tail}
                          </pre>
                        </>
                      )}
                    </div>
                    {caseDetail && (
                      <details className="generation-previous-case" open>
                        <summary>
                          <span>{t("Previous cases and steps")}</span>
                          <HelpHint message={t("The previous revision is retained for comparison while changes are generated.")} />
                        </summary>
                        <TestCaseFlow
                          detail={caseDetail}
                          cleanupEnabled={cleanupTestData}
                          execution={null}
                          executionComplete={false}
                          executionFailed={false}
                          compact
                        />
                      </details>
                    )}
                    {generationIsLive && (
                      <button
                        type="button"
                        className="danger-action full-width"
                        disabled={currentRunActivity?.title === "Terminating generation"}
                        onClick={onCancelGeneration}
                      >
                        {currentRunActivity?.title === "Terminating generation" ? t("Terminating…") : t("Terminate generation")}
                      </button>
                    )}
                  </section>
                  <aside className="dossier">
                    <div className="dossier-heading">
                      <p className="section-kicker">{t("Task snapshot")}</p>
                      <span>BOUND</span>
                    </div>
                    <dl className="definition-list compact">
                      <div>
                        <dt>{t("Objective")}</dt>
                        <dd>{objective}</dd>
                      </div>
                      <div>
                        <dt>{t("Acceptance criteria")}</dt>
                        <dd>{t("{count} criteria", { count: criteriaText.split("\n").filter(Boolean).length })}</dd>
                      </div>
                      <div>
                        <dt>{t("Strategy")}</dt>
                        <dd>
                          {testStrategy === "e2e_api_flow"
                            ? t("API E2E")
                            : testStrategy === "integration"
                              ? t("Integration test")
                              : t("Unit test")}
                        </dd>
                      </div>
                      <div>
                        <dt>{t("Test data")}</dt>
                        <dd>{cleanupTestData ? t("Clean up after execution") : t("Retain named isolation")}</dd>
                      </div>
                      <div>
                        <dt>{t("Current task")}</dt>
                        <dd>{generationIsLive ? t("Generation and review") : t("Waiting to resume generation")}</dd>
                      </div>
                    </dl>
                    {!generationIsLive && (
                      <button
                        type="button"
                        className="primary-action"
                        disabled={busy || !objective.trim() || !agentReady}
                        onClick={() => void (generationRetryAvailable ? onRetryGeneration() : onGenerate())}
                      >
                        {generationRetryAvailable ? t("Regenerate tests") : t("Continue generating tests")}{" "}
                        <span>→</span>
                      </button>
                    )}
                  </aside>
                </div>
              ) : !caseDetail ? (
                <div className="task-definition-workspace">
                  <section className="workspace-panel task-composer">
                    <div className="panel-heading">
                      <div>
                        <p className="section-kicker">TASK DEFINITION / 02</p>
                        <h2>{t("Define what this test must prove")}</h2>
                      </div>
                      <span className="panel-index">02</span>
                    </div>
                    <label className="editor-field">
                      <span>{t("Test prompt")}</span>
                      <textarea
                        rows={4}
                        value={objective}
                        disabled={busy}
                        onChange={(event) => setObjective(event.target.value)}
                        placeholder={t("Describe the business flow, risks, and what the test must prove")}
                      />
                    </label>
                    <label className="editor-field">
                      <span>{t("Acceptance criteria · one per line")}</span>
                      <textarea
                        rows={5}
                        value={criteriaText}
                        disabled={busy}
                        onChange={(event) => setCriteriaText(event.target.value)}
                        placeholder={t("Repeated callbacks are not charged twice\nFailure paths have explicit assertions")}
                      />
                    </label>
                    <details className="environment-panel">
                      <summary>
                        <span>
                          <strong>{t("Test environment")}</strong>
                          <small>{t("Runtime endpoints only; a trusted ServiceProfile controls the service lifecycle")}</small>
                        </span>
                        <span className="environment-summary-status">
                          {testEnvironment.context.trim() || testEnvironment.base_url.trim() ? t("Provided") : t("Optional")}
                        </span>
                      </summary>
                      <label className="editor-field environment-context-field">
                        <span>{t("Environment information")}</span>
                        <textarea
                          rows={7}
                          value={testEnvironment.context}
                          disabled={busy}
                          onChange={(event) =>
                            setTestEnvironment((previous) => ({
                              ...previous,
                              context: event.target.value,
                            }))
                          }
                          placeholder={t("Example:\nService URL: http://localhost:8300\nLogin endpoint: POST /api/login\nVerify MySQL and Redis state changes; test tenant: qa")}
                        />
                      </label>
                      <p className="environment-help">
                        {t("Free-form text is saved with this task and sent to the CLI; avoid putting secrets in it. Username, password, and variables are only stored as fingerprints.")}
                      </p>
                    </details>
                    <div className="composer-options test-strategy-options">
                      <label className="strategy-field">
                        <span>{t("Test strategy")}</span>
                        <select
                          className="strategy-select"
                          value={testStrategy}
                          disabled={busy}
                          onChange={(event) => setTestStrategy(event.target.value)}
                        >
                          <option value="e2e_api_flow">{t("API E2E flow test")}</option>
                          <option value="integration">{t("Integration test")}</option>
                          <option value="unit">{t("Unit test")}</option>
                        </select>
                      </label>
                      <label className="checkbox-field">
                        <input
                          type="checkbox"
                          checked={cleanupTestData}
                          disabled={busy || testStrategy === "unit"}
                          onChange={(event) =>
                            setCleanupTestData(event.target.checked)
                          }
                        />
                        <span>
                          {t("Clean up test data after case execution")}{" "}
                          <HelpHint message={t("E2E/integration tests require the Agent to use the project's cleanup mechanism, such as Cleanup, transactions, delete SQL, or cache keys.")} />
                        </span>
                      </label>
                    </div>
                    <div className="composer-options agent-options">
                      <div className="agent-field-card codex-environment">
                        <span className="agent-field-label">{t("Generation service")}</span>
                        <div className="agent-control-line">
                          <AgentSelect
                            value={agentMode}
                            options={taskAgentOptions}
                            disabled={busy}
                            onChange={onAgentModeChange}
                          />
                          <StateMark
                            tone={agentReady ? "success" : "danger"}
                            label={agentReady ? t("Ready") : t("Configuration required")}
                          />
                        </div>
                        {!agentReady && (
                          <div className="agent-readiness-help">
                            <small className="agent-auth-status">
                              {t(selectedAgent?.auth_status ??
                                selectedAgent?.error ??
                                "Complete generation-service configuration, then refresh status.")}
                            </small>
                            <button
                              type="button"
                              disabled={busy}
                              onClick={refreshCodexEnvironment}
                            >
                              {t("Refresh status")}
                            </button>
                          </div>
                        )}
                      </div>
                        {agentMode !== "mock" && (
                          <div className="agent-field-card">
                            <span className="agent-field-label">{t("Model")}</span>
                            <AgentSelect
                              value={agentModel}
                              options={agentModelOptions}
                              disabled={busy || agentModelSaving}
                              onChange={(value) => void onAgentModelChange(value)}
                            />
                          </div>
                        )}
                      </div>
                    <label className="checkbox-field session-reuse-field">
                      <input
                        type="checkbox"
                        checked={reuseAgentSession}
                        disabled={busy}
                        onChange={(event) => setReuseAgentSession(event.target.checked)}
                      />
                      <span>{t("Reuse the same AI session")}</span>
                      <small>
                        {t("Session reuse only applies to generation and automated revisions; review and result analysis use independent short sessions.")}
                      </small>
                    </label>
                    <div className="primary-action-row">
                      <p>
                        {t("Generation is followed by case review.")}{" "}
                        <HelpHint message={t("The system freezes the candidate and automatically starts AI case review when generation completes.")} />
                      </p>
                      <button
                        type="button"
                        className="primary-action"
                        disabled={busy || !objective.trim() || !agentReady}
                        onClick={() => void (generationRetryAvailable ? onRetryGeneration() : onGenerate())}
                      >
                        {generationRetryAvailable ? t("Regenerate tests") : t("Generate test candidate")}{" "}
                        <span>→</span>
                      </button>
                    </div>
                  </section>
                </div>
              ) : execution && !executionComplete ? (
                <div className="execution-workbench">
                  <section className="workspace-panel execution-main">
                    <div className="panel-heading">
                      <div>
                        <p className="section-kicker">EXECUTION EVIDENCE / 05</p>
                        <h2>{phaseLabel(execution.outcome.phase)}</h2>
                      </div>
                      <StateMark
                        tone={
                          execution.outcome.phase === "running" ||
                          execution.outcome.phase === "starting"
                            ? "active"
                            : execution.outcome.phase === "exited"
                              ? "success"
                              : "danger"
                        }
                        label={
                          execution.outcome.exit_code == null
                            ? phaseLabel(execution.outcome.phase)
                            : `退出码 ${execution.outcome.exit_code}`
                        }
                      />
                    </div>
                    <div className="command-identity">
                      <span>{t("Confirmed test configuration")}</span>
                      <code>local-runner / candidate-tree / structured-output</code>
                    </div>
                    <TestCaseFlow
                      detail={caseDetail}
                      cleanupEnabled={cleanupTestData}
                      execution={execution}
                      executionComplete={executionProcessComplete}
                      executionFailed={executionProcessFailed}
                    />
                    <div className="progress-track execution-progress" aria-hidden="true">
                      <span />
                    </div>
                    <div className="test-tree">
                      <div className="table-heading">
                        <span>{t("Structured test inventory")}</span>
                        <span>{testRows.length ? t("{count} observed", { count: testRows.length }) : t("Waiting for test events")}</span>
                      </div>
                      {testRows.length > 0 ? (
                        <ul>
                          {testRows.map((row) => (
                            <li key={`${row.pkg}-${row.name}`}>
                              <StateMark
                                tone={resultTone(row.action)}
                                label={resultActionLabel(row.action)}
                              />
                              <code>{row.name}</code>
                              <span>{row.pkg}</span>
                              <time>{row.elapsed == null ? "—" : `${row.elapsed.toFixed(2)}s`}</time>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <div className="table-empty">
                          {t("Structured test events appear here when emitted by the executor; raw output is always retained.")}
                        </div>
                      )}
                    </div>
                    <div className="terminal-grid">
                      <div>
                        <span>{t("STDOUT / raw evidence")}</span>
                        <pre>{execution.stdout_tail || t("Waiting for test output…")}</pre>
                      </div>
                      <div>
                        <span>{t("STDERR / diagnostic evidence")}</span>
                        <pre>{execution.stderr_tail || t("No error output")}</pre>
                      </div>
                    </div>
                  </section>
                  <aside className="dossier execution-dossier">
                    <div className="dossier-heading">
                      <p className="section-kicker">{t("Execution dossier")}</p>
                      <span>LIVE</span>
                    </div>
                    <dl className="definition-list compact">
                      <div>
                        <dt>{t("Approval status")}</dt>
                        <dd>{approvedCase ? t("Approved and bound") : t("Not approved")}</dd>
                      </div>
                      <div>
                        <dt>Trust</dt>
                        <dd>{trusted ? t("Confirmed") : t("Preparing")}</dd>
                      </div>
                      <div>
                        <dt>{t("Intended tests")}</dt>
                        <dd>{caseDetail.intended_count || t("Collecting")}</dd>
                      </div>
                      <div>
                        <dt>{t("Start time")}</dt>
                        <dd>{execution.outcome.started_at}</dd>
                      </div>
                      <div>
                        <dt>{t("Process duration")}</dt>
                        <dd>{formatProcessDuration(execution.outcome)}</dd>
                      </div>
                    </dl>
                    <div className="dossier-callout">
                      <strong>
                        {t("Managed process")} <HelpHint message={t("Stopping terminates the test process group and retains current output as evidence.")} />
                      </strong>
                    </div>
                    <button
                      type="button"
                      className="danger-action full-width"
                      disabled={currentRunActivity?.title === "Stopping test"}
                      onClick={onCancelExecution}
                    >
                      {t("Stop execution")}
                    </button>
                  </aside>
                </div>
              ) : executionComplete && execution ? (
                <div className="result-workbench">
                  <section className="workspace-panel result-main">
                    <div className="panel-heading">
                      <div>
                        <p className="section-kicker">EXECUTION REPORT / 06</p>
                        <h2>{t("Test execution report")}</h2>
                      </div>
                      <StateMark
                        tone={executionFailed ? "danger" : canAcceptPass ? "success" : "warning"}
                        label={executionFailed ? t("Tests failed") : canAcceptPass ? t("Pass guard open") : t("Human decision required")}
                      />
                    </div>

                    <div
                      className={`verdict-banner ${
                        executionFailed ? "failed" : canAcceptPass ? "passed" : "pending"
                      }`}
                    >
                      <p>DETERMINISTIC RESULT</p>
                      <strong>
                        {executionFailed
                          ? t("Tests failed")
                          : canAcceptPass
                            ? t("All guards passed")
                            : t("Evidence is insufficient for acceptance")}
                      </strong>
                      <span>
                        {execution.generic_result?.summary ??
                          t("Process {phase} · exit code {code}", { phase: phaseLabel(execution.outcome.phase), code: execution.outcome.exit_code ?? "—" })}
                      </span>
                    </div>

                    <TestCaseFlow
                      detail={caseDetail}
                      cleanupEnabled={cleanupTestData}
                      execution={execution}
                      executionComplete={executionComplete}
                      executionFailed={executionFailed}
                    />

                    <div className="result-metrics">
                      <div>
                        <span>{t("Total tests")}</span>
                        <strong>{execution.structured?.total ?? "—"}</strong>
                      </div>
                      <div>
                        <span>{t("Passed")}</span>
                        <strong className="success-text">
                          {execution.structured?.passed ?? "—"}
                        </strong>
                      </div>
                      <div>
                        <span>{t("Failed")}</span>
                        <strong className="danger-text">
                          {execution.structured?.failed ?? "—"}
                        </strong>
                      </div>
                      <div>
                        <span>{t("Exit code")}</span>
                        <strong>{execution.outcome.exit_code ?? "—"}</strong>
                      </div>
                      <div>
                        <span>{t("Process duration")}</span>
                        <strong>{formatProcessDuration(execution.outcome)}</strong>
                      </div>
                    </div>

                    <div className="evidence-section">
                      <div className="table-heading">
                        <span>{t("Executed test identities")}</span>
                        <span>{t("{count} items", { count: testRows.length || caseDetail.intended_count })}</span>
                      </div>
                      {testRows.length > 0 ? (
                        <ul className="evidence-tests">
                          {testRows.map((row) => (
                            <li key={`${row.pkg}-${row.name}`}>
                              <StateMark tone={resultTone(row.action)} label={resultActionLabel(row.action)} />
                              <code>{row.name}</code>
                              <span>{row.pkg}</span>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <p className="table-empty">
                          {t("{count} intended test identities recorded; the executor did not return per-test JSON.", { count: caseDetail.intended_count })}
                        </p>
                      )}
                    </div>

                    <details className="evidence-terminal">
                      <summary>{t("View raw stdout / stderr evidence")}</summary>
                      <div className="terminal-grid">
                        <div>
                          <span>STDOUT</span>
                          <pre>{execution.stdout_tail || t("No output")}</pre>
                        </div>
                        <div>
                          <span>STDERR</span>
                          <pre>{execution.stderr_tail || t("No output")}</pre>
                        </div>
                      </div>
                    </details>
                  </section>

                  <aside className="dossier decision-dossier">
                    <div className="dossier-heading">
                      <p className="section-kicker">{t("Decision guards")}</p>
                      <span>{canAcceptPass ? "OPEN" : "BLOCKED"}</span>
                    </div>
                    <ul className="guard-list">
                      <li>
                        <StateMark
                          tone={
                            execution.outcome.phase === "exited" &&
                            execution.outcome.exit_code === 0
                              ? "success"
                              : "danger"
                          }
                          label={t("Process exited successfully")}
                        />
                      </li>
                      <li>
                        <StateMark
                          tone={(execution.structured?.failed ?? 1) === 0 ? "success" : "danger"}
                          label={t("Structured result has no failures")}
                        />
                      </li>
                      <li>
                        <StateMark
                          tone={approvedCase ? "success" : "danger"}
                          label={t("Case approved and bound")}
                        />
                      </li>
                      <li>
                        <StateMark
                          tone={caseDetail.candidate ? "success" : "danger"}
                          label={t("Candidate is traceable")}
                        />
                      </li>
                      <li>
                        <StateMark
                          tone={canAcceptPass ? "success" : "warning"}
                          label={t("Apply guard")}
                        />
                      </li>
                    </ul>
                    {execution.accept_pass?.reasons &&
                      execution.accept_pass.reasons.length > 0 && (
                        <div className="block-reasons">
                          <strong>{t("Blocking reasons")}</strong>
                          {execution.accept_pass.reasons.map((reason) => (
                            <p key={reason}>{reason}</p>
                          ))}
                        </div>
                      )}
                    {(analysisInfo || resultReviewInfo) && (
                      <div className="agent-advice">
                        <p className="section-kicker">{t("AI analysis")}</p>
                        {analysisInfo && <p>{analysisInfo}</p>}
                        {resultReviewInfo && <p>{resultReviewInfo}</p>}
                      </div>
                    )}
                    {!analysisInfo && !resultReviewInfo && canAcceptPass && (
                      <div className="decision-note">
                        {t("Analyzer / Result Reviewer evidence is not loaded. The backend will not allow AcceptPass until both complete.")}
                      </div>
                    )}
                    {acceptInfo && <div className="decision-note">{acceptInfo}</div>}
                    {!readyToApply ? (
                      <button
                        type="button"
                        className="primary-action full-width"
                        disabled={busy || !canAcceptPass}
                        onClick={onAcceptPass}
                      >
                        {t("Accept pass")} <span>→</span>
                      </button>
                    ) : (
                      <button
                        type="button"
                        className="primary-action full-width"
                        disabled={busy}
                        onClick={onApplyAcceptedRevision}
                      >
                        {t("Apply to repository")} <span>→</span>
                      </button>
                    )}
                    <label className="editor-field">
                      <span>{t("Result-stage test-change feedback")}</span>
                      <textarea
                        rows={3}
                        value={changeRequest}
                        disabled={busy || readyToApply}
                        onChange={(event) => setChangeRequest(event.target.value)}
                        placeholder={t("Describe required test changes, coverage gaps, or evidence issues")}
                      />
                    </label>
                    <button
                      type="button"
                      disabled={busy || readyToApply || !changeRequest.trim()}
                      onClick={() => void onRequestResultTestChange()}
                    >
                      {t("Change tests and create a new Revision")}
                    </button>
                    <div className="secondary-actions">
                      <button type="button" disabled={busy} onClick={() => void onRerunExecution()}>
                        {t("Rerun")}
                      </button>
                      <button type="button" disabled={busy} onClick={onExport}>
                        {t("Export patch")}
                      </button>
                      <button type="button" disabled={busy} onClick={onCreateSuccessor}>
                        {t("Continue from latest code")}
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => onResultDecision("result_accept_fail")}
                      >
                        {t("Record failure")}
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => onResultDecision("result_mark_bug")}
                      >
                        {t("Mark product defect")}
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => onResultDecision("result_mark_env")}
                      >
                        {t("Mark environment issue")}
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => onResultDecision("result_mark_flaky")}
                      >
                        {t("Mark Flaky")}
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => onResultDecision("result_end_no_decision")}
                      >
                        {t("Retain evidence and end")}
                      </button>
                    </div>
                  </aside>
                </div>
              ) : (
                <div
                  className={`review-workbench${reviewDecisionOpen ? " decision-open" : ""}`}
                >
                  <section className="review-diff">
                    <div className="diff-toolbar">
                      <div>
                        <p className="section-kicker">CASE CONTRACT / 04</p>
                        <h2>{t("Test step contract")}</h2>
                      </div>
                    </div>
                    <div className="review-flow-toolbar review-flow-toolbar-focus">
                      <div className="review-flow-mode">
                        <span>{t("Flow focus")}</span>
                        <HelpHint message={t("Open a step card to inspect its input, action, output, and assertion.")} />
                      </div>
                      <button
                        type="button"
                        className={reviewDecisionOpen ? "active" : ""}
                        aria-expanded={reviewDecisionOpen}
                        onClick={() => setReviewDecisionOpen((open) => !open)}
                      >
                        <span>{t("Review dossier")}</span>
                        <small>{t("{count} findings", { count: caseDetail.review.findings.length })}</small>
                      </button>
                    </div>
                    <TestCaseFlow
                      detail={caseDetail}
                      cleanupEnabled={cleanupTestData}
                      execution={execution}
                      executionComplete={executionComplete}
                      executionFailed={executionFailed}
                      compact
                    />
                    {caseDetail.candidate && (
                      <footer className="diff-binding">
                        <span>
                          BASE <code>{caseDetail.candidate.base_commit.slice(0, 12)}</code>
                        </span>
                        <span>→</span>
                        <span>
                          CANDIDATE{" "}
                          <code>{caseDetail.candidate.candidate_commit.slice(0, 12)}</code>
                        </span>
                      </footer>
                    )}
                  </section>

                  <aside className="review-decision">
                    <div className="dossier-heading">
                      <p className="section-kicker">{t("Review dossier")}</p>
                        <span>{caseDetail.approved ? t("Approved") : t("Pending")}</span>
                    </div>
                    <div className="review-summary-block">
                      <StateMark
                        tone={
                          caseDetail.approved
                            ? "success"
                            : caseDetail.can_approve
                              ? "warning"
                              : "danger"
                        }
                        label={
                          caseDetail.approved
                            ? t("Case approved")
                            : caseDetail.can_approve
                              ? t("Can approve")
                              : t("Blocking findings")
                        }
                      />
                      <h3>{caseDetail.review.summary}</h3>
                      <p>
                        {t("Review recommendation")}{" "}
                        <strong>{reviewRecommendationLabel(caseDetail.review.recommendation)}</strong> ·{" "}
                        {t("{count} intended tests", { count: caseDetail.intended_count })}
                      </p>
                    </div>

                    <div className="findings-section">
                      <div className="column-heading">
                        <p className="section-kicker">{t("Review findings")}</p>
                        <span>{caseDetail.review.findings.length}</span>
                      </div>
                      {caseDetail.review.findings.length === 0 ? (
                        <div className="no-findings">
                          <StateMark tone="success" label={t("No blocking issues found")} />
                        </div>
                      ) : (
                        <ul className="findings-list">
                          {[...caseDetail.review.findings]
                            .sort(
                              (left, right) =>
                                reviewSeverityRank(left.severity) -
                                reviewSeverityRank(right.severity),
                            )
                            .map((finding) => {
                              const disposition = normDisp(finding.disposition);
                              return (
                                <li key={finding.id} className={`finding disp-${disposition}`}>
                                  <div className="finding-head">
                                    <span>{reviewSeverityLabel(finding.severity)}</span>
                                    <code>
                                      {finding.file ?? finding.category}
                                      {finding.line == null ? "" : `:${finding.line}`}
                                    </code>
                                  </div>
                                  <p>{finding.message}</p>
                                  {finding.suggestion && <small>{finding.suggestion}</small>}
                                  <label>
                                    <span>{t("Disposition")}</span>
                                    <ReviewDispositionSelect
                                      value={disposition}
                                      disabled={
                                        busy ||
                                        (caseDetail.approved && caseDetail.workflow_state !== "NeedsHuman")
                                      }
                                      onChange={(value) => onSetDisposition(finding.id, value)}
                                    />
                                  </label>
                                  {disposition === "accepted_risk" && (
                                    <input
                                      value={
                                        riskDrafts[finding.id] ?? finding.risk_reason ?? ""
                                      }
                                      disabled={
                                        busy ||
                                        (caseDetail.approved && caseDetail.workflow_state !== "NeedsHuman")
                                      }
                                      placeholder={t("Explain why this risk is accepted")}
                                      onChange={(event) =>
                                        setRiskDrafts((previous) => ({
                                          ...previous,
                                          [finding.id]: event.target.value,
                                        }))
                                      }
                                      onBlur={() =>
                                        !caseDetail.approved &&
                                        onSetDisposition(finding.id, "accepted_risk")
                                      }
                                    />
                                  )}
                                </li>
                              );
                            })}
                        </ul>
                      )}
                    </div>

                    {!caseDetail.can_approve && !caseDetail.approved && (
                      <div className="block-reasons">
                        <strong>{t("Approval blockers")}</strong>
                        {caseDetail.approve_blockers.map((blocker) => (
                          <p key={blocker}>{blocker}</p>
                        ))}
                      </div>
                    )}

                    {caseDetail.pending_change_request ? (
      <div className="change-request">
                        <div className="decision-note">
                          <strong>{t("Pending human change request")}</strong>
                          <p>{caseDetail.pending_change_request}</p>
                        </div>
                        <button
                          type="button"
                          className="primary-action full-width"
                          disabled={busy || !agentReady}
                          onClick={() => void onRetryGeneration(false)}
                        >
                          {t("Generate the next revision")} <span>→</span>
                        </button>
                      </div>
                    ) : caseDetail.approved && caseDetail.workflow_state === "ExecutionReady" ? (
                      <>
                        <button
                          type="button"
                          className="primary-action full-width"
                          disabled={busy}
                          onClick={() => void onTrustAndRun()}
                        >
                          {t("Start formal execution")} <span>→</span>
                        </button>
                      </>
                    ) : caseDetail.approved && caseDetail.workflow_state === "NeedsHuman" ? (
                      <div className="block-reasons human-review-required">
                        <strong>{t("Human review required")}</strong>
                        <p>{t("Resolve or accept the review findings, approve the case again, and then start formal execution.")}</p>
                      </div>
                    ) : caseDetail.can_approve ? (
                      <button
                        type="button"
                        className="primary-action full-width"
                        disabled={busy}
                        onClick={onApproveCase}
                      >
                        {t("Approve current case")} <span>→</span>
                      </button>
                    ) : caseDetail.can_retry_generation ? (
                      <button
                        type="button"
                        className="primary-action full-width"
                          disabled={busy || !agentReady}
                        onClick={() => void onRetryGeneration(true)}
                      >
                        {t("Regenerate tests")} <span>→</span>
                      </button>
                    ) : (
                      <button
                        type="button"
                        className="primary-action full-width"
                        disabled={busy || !caseDetail.can_approve}
                        onClick={onApproveCase}
                      >
                        {t("Approve current case")} <span>→</span>
                      </button>
                    )}
                    {!caseDetail.approved && (
                      <div className="change-request">
                        <label>
                          <span>{t("Change request / rejection reason")}</span>
                          <textarea
                            rows={3}
                            value={changeRequest}
                            disabled={busy}
                            onChange={(event) => setChangeRequest(event.target.value)}
                            placeholder={t("Describe what must change in behavior or assertions; give a clear reason when rejecting…")}
                          />
                        </label>
                        <button
                          type="button"
                          disabled={busy || !changeRequest.trim()}
                          onClick={onRequestChanges}
                        >
                          {t("Submit change request")}
                        </button>
                        <button
                          type="button"
                          disabled={busy || !changeRequest.trim()}
                          onClick={onCommentCase}
                        >
                          {t("Add comment only")}
                        </button>
                        <button
                          type="button"
                          className="danger-action"
                          disabled={busy || !changeRequest.trim()}
                          onClick={onRejectCase}
                        >
                          {t("Reject current case")}
                        </button>
                      </div>
                    )}
                  </aside>
                </div>
              )}

              {auditPanel}
            </div>
          ))}

        </main>
      </div>

      {settingsOpen && (
        <div
          className="settings-modal-backdrop"
          role="presentation"
          onMouseDown={() => setSettingsOpen(false)}
        >
          <section
            className="settings-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <header className="settings-modal-header">
              <div>
                <p className="section-kicker">LOCAL SYSTEM</p>
                <h2 id="settings-title">{t("System settings")}</h2>
                <p>{t("Manage generation services, local storage, and recovery.")}</p>
              </div>
              <button
                type="button"
                className="settings-close"
                onClick={() => setSettingsOpen(false)}
                aria-label={t("Close system settings")}
                autoFocus
              >
                ×
              </button>
            </header>

            <div className="settings-modal-body">
              <section className="settings-modal-section">
                <div className="settings-section-heading">
                  <div>
                    <p className="section-kicker">LANGUAGE</p>
                    <h3>{t("Language")}</h3>
                  </div>
                  <select
                    value={language}
                    onChange={(event) => void onLanguageChange(event.target.value as Language)}
                    aria-label={t("Language")}
                  >
                    <option value="en">English</option>
                    <option value="zh-CN">简体中文</option>
                  </select>
                </div>
              </section>

              <section className="settings-modal-section">
                <div className="settings-section-heading">
                  <div>
                    <p className="section-kicker">{t("Runtime")}</p>
                    <h3>{t("Application and generation services")}</h3>
                  </div>
                  <StateMark
                    tone={info?.domain_ready ? "success" : "warning"}
                    label={info?.domain_ready ? t("Core domain ready") : t("Core domain unavailable")}
                  />
                </div>
                <dl className="definition-list">
                  <div>
                    <dt>{t("TestHarbor version")}</dt>
                    <dd>{info?.version ?? "—"}</dd>
                  </div>
                  <div>
                    <dt>{t("Generation service")}</dt>
                    <dd>{agentReady ? t("{service} ready", { service: agentLabel }) : t("{service} requires configuration", { service: agentLabel })}</dd>
                  </div>
                  <div>
                    <dt>{t("Service version")}</dt>
                    <dd>{selectedAgent?.version ?? t("Not detected")}</dd>
                  </div>
                  <div>
                    <dt>{t("Data directory")}</dt>
                    <dd className="path-value">
                      {info?.data_dir ?? usage?.data_dir ?? "—"}
                    </dd>
                  </div>
                </dl>
                <div className="settings-agent-config">
                  <div className="settings-agent-config-heading">
                    <div>
                      <p className="section-kicker">{t("Generation service configuration")}</p>
                      <strong>{t("Tasks only show enabled and ready CLIs")}</strong>
                    </div>
                    <span>{t("{count} available", { count: availableAgentOptions.length })}</span>
                  </div>
                  <div className="settings-agent-list">
                    {[
                      { mode: "codex", label: "Codex CLI", environment: codex },
                      { mode: "claude", label: "Claude Code CLI", environment: claude },
                      { mode: "grok", label: "Grok CLI", environment: grok },
                    ].map(({ mode, label, environment }) => {
                      const ready = Boolean(environment?.ready);
                      const installed = cliInstalled(environment);
                      const enabled = agentPreferences.enabled_modes.includes(mode);
                      return (
                        <div className="settings-agent-row" key={mode}>
                          <div>
                            <strong>{label}</strong>
                            <small>
                              {!installed
                                ? t("Not installed")
                                : ready
                                ? `${environment?.version ?? t("Detected")} · ${enabled ? t("Enabled") : t("Disabled")}`
                                : t(environment?.auth_status ?? environment?.error ?? "Installed but not ready")}
                            </small>
                          </div>
                          <div className="settings-agent-actions">
                            {!installed && (
                              <button
                                type="button"
                                className="settings-agent-install"
                                onClick={() => void openCliInstallGuide(mode)}
                              >
                                {t("Install guide ↗")}
                              </button>
                            )}
                            <label className="settings-agent-toggle">
                              <input
                                type="checkbox"
                                checked={enabled}
                                disabled={busy || !ready}
                                onChange={(event) =>
                                  void onAgentPreferenceChange(mode, event.target.checked)
                                }
                              />
                              <span>
                                {!installed ? t("Not installed") : enabled && ready ? t("Enabled") : ready ? t("Disabled") : t("Unavailable")}
                              </span>
                            </label>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              </section>

              <section className="settings-modal-section">
                <div className="settings-section-heading">
                  <div>
                    <p className="section-kicker">{t("Local usage")}</p>
                    <h3>{usage ? fmtBytes(usage.total_bytes) : t("Reading")}</h3>
                  </div>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={async () =>
                      setUsage(await invoke<DiskUsage>("get_disk_usage"))
                    }
                  >
                    {t("Refresh")}
                  </button>
                </div>
                {usage && (
                  <div className="storage-bars">
                    {[
                      ["Worktrees", usage.worktrees_bytes],
                      ["Artifacts", usage.artifacts_bytes],
                      ["Runs", usage.runs_bytes],
                      ["Database", usage.db_bytes],
                    ].map(([label, value]) => {
                      const bytes = Number(value);
                      const width =
                        usage.total_bytes === 0
                          ? 0
                          : Math.max(2, (bytes / usage.total_bytes) * 100);
                      return (
                        <div key={String(label)}>
                          <span>
                            {label} <b>{fmtBytes(bytes)}</b>
                          </span>
                          <i style={{ width: `${width}%` }} />
                        </div>
                      );
                    })}
                  </div>
                )}
              </section>

              <section className="settings-modal-section settings-maintenance">
                <div>
                  <p className="section-kicker">{t("Maintenance")}</p>
                  <h3>{t("Local data and recovery")}</h3>
                  <p>
                    {settingsMsg ??
                      t("Cleanup never deletes active runs or changes project repositories.")}
                  </p>
                </div>
                <div className="maintenance-actions">
                  <button
                    type="button"
                    className="primary-action"
                    disabled={busy}
                    onClick={onCleanup}
                  >
                    {t("Clear worktrees and build caches")}
                  </button>
                  <button type="button" disabled={busy} onClick={onRecover}>
                    {t("Run recovery check")}
                  </button>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={refreshCodexEnvironment}
                  >
                    {t("Refresh status")}
                  </button>
                </div>
              </section>
            </div>
          </section>
        </div>
      )}
    </div>
  );
}

export default App;
