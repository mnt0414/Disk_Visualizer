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
