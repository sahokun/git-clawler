import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn, type Event } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import {
  EVENT_REPOS_UPDATED,
  EVENT_REPOS_ERROR,
  EVENT_SETTINGS_OPEN,
  EVENT_SHORTCUT_STATUS,
  type AppConfigDto,
  type ListRepositoriesResponse,
  type RepoEntryDto,
  type RealtimeErrorPayload,
  type ShortcutStatusPayload,
} from './types';

export async function listRepositories(): Promise<ListRepositoriesResponse> {
  return invoke<ListRepositoriesResponse>('list_repositories');
}

export async function requestRefresh(): Promise<void> {
  return invoke<void>('request_refresh');
}

export async function getStartupErrors(): Promise<RealtimeErrorPayload[]> {
  return invoke<RealtimeErrorPayload[]>('get_startup_errors');
}

export function subscribeRepositories(
  cb: (e: Event<ListRepositoriesResponse>) => void,
): Promise<UnlistenFn> {
  return listen<ListRepositoriesResponse>(EVENT_REPOS_UPDATED, cb);
}

export function subscribeRepositoriesError(
  cb: (e: Event<RealtimeErrorPayload>) => void,
): Promise<UnlistenFn> {
  return listen<RealtimeErrorPayload>(EVENT_REPOS_ERROR, cb);
}

export function subscribeShortcutStatus(
  cb: (e: Event<ShortcutStatusPayload>) => void,
): Promise<UnlistenFn> {
  return listen<ShortcutStatusPayload>(EVENT_SHORTCUT_STATUS, cb);
}

/** Backend changed `repositories` on disk (discovery or removal). */
export function subscribeConfigRepositoriesChanged(cb: () => void): Promise<UnlistenFn> {
  return listen<null>('config://repositories-changed', () => cb());
}

/** Remove a registered repository (never deletes files); may add a scan-root exclusion. */
export async function removeRepository(
  path: string,
  distro: string | null,
): Promise<{ excluded: [string, string] | null }> {
  return invoke<{ excluded: [string, string] | null }>('remove_repository', { path, distro });
}

export function subscribeSettingsOpen(cb: () => void): Promise<UnlistenFn> {
  return listen<null>(EVENT_SETTINGS_OPEN, () => cb());
}

export async function getConfig(): Promise<AppConfigDto> {
  return invoke<AppConfigDto>('get_config');
}

export async function saveConfig(config: AppConfigDto): Promise<void> {
  return invoke<void>('save_config', { config });
}

export async function saveColumnWidths(
  columnWidths: Record<string, number>,
): Promise<AppConfigDto> {
  return invoke<AppConfigDto>('save_column_widths', { columnWidths });
}

export async function scanRoot(
  root: string,
  depth: number,
  exclude: string[],
  distro: string | null,
): Promise<RepoEntryDto[]> {
  return invoke<RepoEntryDto[]>('scan_root', { root, depth, exclude, distro });
}

export async function pickDirectory(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false });
  return typeof result === 'string' ? result : null;
}

export async function openRepositoryFolder(path: string, distro: string | null): Promise<void> {
  return invoke<void>('open_repository_folder', { path, distro });
}

export async function openGithubPage(remoteUrl: string): Promise<void> {
  return invoke<void>('open_github_page', { remoteUrl });
}
