import { invoke } from '@tauri-apps/api/core'
import type { ScanJobSnapshot, ScanSummary } from '../types/scan'

function requireDesktop() {
  if (!('__TAURI_INTERNALS__' in window)) {
    throw new Error('フォルダのスキャンはデスクトップアプリで利用できます')
  }
}

function normalizePath(path: string) {
  const normalizedPath = path.trim()
  if (!normalizedPath) throw new Error('スキャンするフォルダを指定してください')
  return normalizedPath
}

export async function scanFolder(path: string): Promise<ScanSummary> {
  requireDesktop()
  return invoke<ScanSummary>('scan_folder', { path: normalizePath(path) })
}

export async function startScan(path: string): Promise<ScanJobSnapshot> {
  requireDesktop()
  return invoke<ScanJobSnapshot>('start_scan', { path: normalizePath(path) })
}

export async function getScanStatus(id: number): Promise<ScanJobSnapshot> {
  requireDesktop()
  return invoke<ScanJobSnapshot>('get_scan_status', { id })
}

export async function pauseScan(id: number): Promise<ScanJobSnapshot> {
  requireDesktop()
  return invoke<ScanJobSnapshot>('pause_scan', { id })
}

export async function resumeScan(id: number): Promise<ScanJobSnapshot> {
  requireDesktop()
  return invoke<ScanJobSnapshot>('resume_scan', { id })
}

export async function cancelScan(id: number): Promise<ScanJobSnapshot> {
  requireDesktop()
  return invoke<ScanJobSnapshot>('cancel_scan', { id })
}
