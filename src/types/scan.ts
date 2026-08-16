export type ScanEntry = {
  name: string
  path: string
  sizeBytes: number
  allocatedSizeBytes: number
  fileCount: number
  directoryCount: number
  skippedCount: number
  hardLinkDuplicateCount: number
  sparseFileCount: number
  compressedFileCount: number
  isDirectory: boolean
}
export type ScanSummary = {
  rootPath: string
  totalSizeBytes: number
  allocatedSizeBytes: number
  fileCount: number
  directoryCount: number
  skippedCount: number
  hardLinkDuplicateCount: number
  sparseFileCount: number
  compressedFileCount: number
  elapsedMilliseconds: number
  entries: ScanEntry[]
  entriesTruncated: boolean
}
export type ScanJobStatus =
  'running' | 'paused' | 'completed' | 'cancelled' | 'failed'
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
export type SavedScan = {
  id: number
  rootPath: string
  totalSizeBytes: number
  fileCount: number
  directoryCount: number
  skippedCount: number
  completedAt: number
}

export type CacheConfidence = 'high' | 'medium' | 'low'
export type CacheCategory = 'browser' | 'media' | 'operatingSystem'
export type CacheRuntimeState = 'stable' | 'changing' | 'unknown'
export type CacheDefinition = {
  id: string
  definitionVersion: number
  platform: 'macos' | 'windows'
  applicationName: string
  versionConstraint: string
  category: CacheCategory
  path: { root: string; relativePath: string; source: string }
  confidence: CacheConfidence
  evidence: string[]
  regenerable: boolean
  cleanupImpact: string
}
export type CacheEntryDetail = {
  id: number
  scanId: number
  name: string
  path: string
  sizeBytes: number
  logicalSize: number
  allocatedSize: number | null
  modifiedAt: number | null
  cacheCatalogVersion: string
  cacheDefinitionId: string
  cacheDefinitionVersion: number
  runtimeState: CacheRuntimeState | null
  definition: CacheDefinition | null
}
