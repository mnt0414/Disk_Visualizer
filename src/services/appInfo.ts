import { invoke } from '@tauri-apps/api/core'
import type { AppInfo } from '../types/app'

const browserFallback: AppInfo = {
  name: 'Disk Visualizer',
  version: 'web-preview',
  platform: 'browser',
  architecture: 'unknown',
}

export async function loadAppInfo(): Promise<AppInfo> {
  if (!('__TAURI_INTERNALS__' in window)) {
    return browserFallback
  }

  return invoke<AppInfo>('get_app_info')
}
