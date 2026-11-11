// DTOs matching crates/gui/src/dto.rs
// RowStatus uses serde tagged union: { kind: "ok"|"err", value: ... }

export interface UnpushedBranchDto {
  name: string;
  ahead: number;
  has_upstream: boolean;
}

export interface RecentCommitDto {
  short_hash: string;
  summary: string;
  author_name: string;
  committed_at: string;
}

export interface RepoStatusDto {
  fetch_warning?: string | null;
  current_branch: string | null;
  uncommitted_files: number;
  unpushed_branches: UnpushedBranchDto[];
  ahead: number;
  behind: number;
  open_issues: number | null;
  open_prs: number | null;
  conflict_predicted: boolean | null;
  recent_commits: RecentCommitDto[];
}

export type RowStatus =
  | { kind: "ok"; value: RepoStatusDto }
  | { kind: "err"; value: string };

export interface RepoRow {
  index: number;
  name: string;
  path: string;
  is_current: boolean;
  scanned_at: string; // RFC 3339
  status: RowStatus;
  distro: string | null;
  remote_url: string | null;
}

export interface ListRepositoriesResponse {
  rows: RepoRow[];
}

export interface CommandError {
  kind: string; // "config" | "internal" | "validation"
  message: string;
}

export const EVENT_REPOS_UPDATED = "repositories://updated";
export const EVENT_REPOS_ERROR = "repositories://error";
export const EVENT_SETTINGS_OPEN = "settings://open";
export const EVENT_SHORTCUT_STATUS = "shortcut://status";

export interface RealtimeErrorPayload {
  kind: string;
  message: string;
  occurred_at: string;
}

export interface ShortcutStatusPayload {
  registered: boolean;
  occurred_at: string;
}

// ---------------------------------------------------------------------------
// AppConfigDto: settings dialog (mirrors crates/gui/src/dto.rs)
// ---------------------------------------------------------------------------

export interface GeneralConfigDto {
  refresh_interval_secs: number;
  auto_fetch: boolean;
}

export interface RepoEntryDto {
  name: string | null;
  path: string;
  fetch: boolean | null;
  distro: string | null;
}

export interface ScanRootEntryDto {
  path: string;
  depth: number;
  exclude: string[];
  distro: string | null;
}

export interface GithubConfigDto {
  token_env: string | null;
  enabled: boolean;
  enable_issues: boolean;
  enable_prs: boolean;
}

export interface NotificationsConfigDto {
  enabled: boolean;
  cooldown_secs: number;
}

export interface GuiConfigDto {
  theme: 'dark' | 'light';
  start_in_tray: boolean;
  hide_from_taskbar: boolean;
  close_to_tray: boolean;
  global_shortcut: string | null;
  window_x: number | null;
  window_y: number | null;
  window_width: number | null;
  window_height: number | null;
  window_maximized: boolean | null;
  column_widths: Record<string, number>;
}

export interface AppConfigDto {
  general: GeneralConfigDto;
  repositories: RepoEntryDto[];
  scan_roots: ScanRootEntryDto[];
  github: GithubConfigDto;
  notifications: NotificationsConfigDto;
  gui: GuiConfigDto;
}
