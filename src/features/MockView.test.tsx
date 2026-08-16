import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { chooseFolder } from '../services/folderPicker'
import { listCacheEntries, listSavedScans, startScan } from '../services/scan'
import { MockView } from './MockView'
vi.mock('../services/folderPicker', () => ({ chooseFolder: vi.fn() }))
vi.mock('../services/scan', () => ({
  startScan: vi.fn(),
  getScanStatus: vi.fn(),
  pauseScan: vi.fn(),
  resumeScan: vi.fn(),
  cancelScan: vi.fn(),
  listSavedScans: vi.fn(),
  listCacheEntries: vi.fn(),
  deleteSavedScan: vi.fn(),
}))
const mockedChooseFolder = vi.mocked(chooseFolder)
const mockedStartScan = vi.mocked(startScan)
const mockedListSavedScans = vi.mocked(listSavedScans)
const mockedListCacheEntries = vi.mocked(listCacheEntries)
beforeEach(() => {
  mockedChooseFolder.mockReset()
  mockedStartScan.mockReset()
  mockedListSavedScans.mockReset()
  mockedListCacheEntries.mockReset()
  mockedListSavedScans.mockResolvedValue([])
  mockedListCacheEntries.mockResolvedValue([])
})
afterEach(cleanup)
const result = {
  rootPath: '/Users/test/Documents',
  totalSizeBytes: 1024,
  allocatedSizeBytes: 4096,
  fileCount: 1,
  directoryCount: 0,
  skippedCount: 0,
  hardLinkDuplicateCount: 0,
  sparseFileCount: 0,
  compressedFileCount: 0,
  elapsedMilliseconds: 2,
  entriesTruncated: false,
  entries: [
    {
      name: 'note.txt',
      path: '/Users/test/Documents/note.txt',
      sizeBytes: 1024,
      allocatedSizeBytes: 4096,
      fileCount: 1,
      directoryCount: 0,
      skippedCount: 0,
      hardLinkDuplicateCount: 0,
      sparseFileCount: 0,
      compressedFileCount: 0,
      isDirectory: false,
    },
  ],
}
const cacheEntry = {
  id: 10,
  scanId: 1,
  name: 'data_1',
  path: '/Users/test/Library/Caches/Google/Chrome/data_1',
  sizeBytes: 1024,
  logicalSize: 1024,
  allocatedSize: 4096,
  modifiedAt: 1723700000,
  cacheCatalogVersion: '2026.08.1',
  cacheDefinitionId: 'chrome.macos.cache',
  cacheDefinitionVersion: 1,
  definition: {
    id: 'chrome.macos.cache',
    definitionVersion: 1,
    platform: 'macos' as const,
    applicationName: 'Google Chrome',
    versionConstraint: '*',
    category: 'browser' as const,
    path: {
      root: 'home',
      relativePath: 'Library/Caches/Google/Chrome',
      source: 'fixed',
    },
    confidence: 'high' as const,
    evidence: ['knownApplicationCachePath', 'platformSpecificPath'],
    regenerable: true,
    cleanupImpact: 'browserMayRebuildCacheAndLoadSlower',
  },
}
describe('MockView', () => {
  it('renders saved application cache evidence', async () => {
    mockedListSavedScans.mockResolvedValue([
      {
        id: 1,
        rootPath: '/Users/test',
        totalSizeBytes: 1024,
        fileCount: 1,
        directoryCount: 0,
        skippedCount: 0,
        completedAt: 1723700000,
      },
    ])
    mockedListCacheEntries.mockResolvedValue([cacheEntry])
    render(
      <MockView viewId="app-cache" label="アプリキャッシュ" description="" />,
    )
    expect(await screen.findByText('Google Chrome')).toBeInTheDocument()
    expect(screen.getAllByText('4.0 KB')).toHaveLength(2)
    expect(mockedListCacheEntries).toHaveBeenCalledWith(1, 100, 0)
    fireEvent.click(screen.getByText('判定根拠と影響'))
    expect(
      screen.getByText('既知のアプリキャッシュパス、OS固有の保存場所'),
    ).toBeInTheDocument()
  })
  it('renders an empty state without saved scans', async () => {
    render(
      <MockView viewId="app-cache" label="アプリキャッシュ" description="" />,
    )
    expect(
      await screen.findByText(/保存済みスキャンがありません/),
    ).toBeInTheDocument()
    expect(mockedListCacheEntries).not.toHaveBeenCalled()
  })
  it('opens the native folder picker', async () => {
    mockedChooseFolder.mockResolvedValue(null)
    render(<MockView viewId="scan" label="スキャン" description="" />)
    fireEvent.click(screen.getByRole('button', { name: 'フォルダを選択' }))
    await waitFor(() => expect(mockedChooseFolder).toHaveBeenCalledOnce())
  })
  it('starts a scan and renders its persisted result', async () => {
    mockedStartScan.mockResolvedValue({
      id: 1,
      path: '/Users/test/Documents',
      status: 'completed',
      currentPath: '',
      totalSizeBytes: 1024,
      fileCount: 1,
      directoryCount: 0,
      skippedCount: 0,
      result,
      error: null,
    })
    render(<MockView viewId="scan" label="スキャン" description="" />)
    const input = screen.getByLabelText('スキャン対象')
    fireEvent.change(input, { target: { value: '/Users/test/Documents' } })
    fireEvent.submit(input.closest('form')!)
    await waitFor(() =>
      expect(screen.getByText('note.txt')).toBeInTheDocument(),
    )
    expect(screen.getAllByText('4.0 KB')).toHaveLength(2)
    expect(mockedStartScan).toHaveBeenCalledWith('/Users/test/Documents')
  })
})
