import { invoke } from '@tauri-apps/api/core'
import type { ScanSummary } from '../types/scan'

export async function scanFolder(path: string): Promise<ScanSummary> {
  const normalizedPath = path.trim()
  if (!normalizedPath) throw new Error('スキャンするフォルダを指定してください')
  if (!('__TAURI_INTERNALS__' in window)) {
    throw new Error('フォルダのスキャンはデスクトップアプリで利用できます')
  }
  return invoke<ScanSummary>('scan_folder', { path: normalizedPath })
}
