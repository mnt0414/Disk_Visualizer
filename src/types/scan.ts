export type ScanEntry = {
  name: string
  path: string
  sizeBytes: number
  fileCount: number
  directoryCount: number
  skippedCount: number
  isDirectory: boolean
}

export type ScanSummary = {
  rootPath: string
  totalSizeBytes: number
  fileCount: number
  directoryCount: number
  skippedCount: number
  elapsedMilliseconds: number
  entries: ScanEntry[]
}

export type ScanJobStatus = 'running' | 'paused' | 'completed' | 'cancelled' | 'failed'

export type ScanJobSnapshot = {
  id: number
  path: string
  status: ScanJobStatus
  currentPath: string
  totalSizeBytes: number
  fileCount: number
  directoryCount: number
  skippedCount: number
  result: ScanSummary | null
  error: string | null
}
