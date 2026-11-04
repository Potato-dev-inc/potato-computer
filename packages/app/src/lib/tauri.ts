import { invoke } from '@tauri-apps/api/core';

export const tauri = {
  appLogLines: (needle: string, max: number) =>
    invoke<string[]>('app_log_lines', { needle, max }).catch(() => []),
};
