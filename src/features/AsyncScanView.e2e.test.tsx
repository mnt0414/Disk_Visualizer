import { act, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { AsyncScanView } from './AsyncScanView'

const scanApi = vi.hoisted(() => ({
  startScan: vi.fn(),
  getScanStatus: vi.fn(),
  pauseScan: vi.fn(),
  resumeScan: vi.fn(),
  cancelScan: vi.fn(),
}))

vi.mock('../services/scan', () => ({
  ...scanApi,
  listSavedScans: vi.fn().mockResolvedValue([]),
  deleteSavedScan: vi.fn(),
}))
vi.mock('../services/folderPicker', () => ({ chooseFolder: vi.fn() }))

const summary = {
  rootPath: '/tmp/sample',
  totalSizeBytes: 1024,
  allocatedSizeBytes: 4096,
  fileCount: 1,
  directoryCount: 0,
  skippedCount: 0,
  hardLinkDuplicateCount: 0,
  sparseFileCount: 0,
  compressedFileCount: 0,
  elapsedMilliseconds: 1,
  entries: [],
  entriesTruncated: false,
}

const running = {
  id: 7,
  path: '/tmp/sample',
  status: 'running' as const,
  currentPath: '/tmp/sample/file.bin',
  totalSizeBytes: 0,
  fileCount: 0,
  directoryCount: 0,
  skippedCount: 0,
  result: null,
  error: null,
}

describe('AsyncScanView desktop flow', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    scanApi.startScan.mockResolvedValue(running)
    scanApi.getScanStatus.mockResolvedValue({
      ...running,
      status: 'completed',
      currentPath: '',
      totalSizeBytes: 1024,
      fileCount: 1,
      result: summary,
    })
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.clearAllMocks()
  })

  it('starts, polls, completes, and renders the persisted result flow', async () => {
    render(<AsyncScanView />)
    fireEvent.change(screen.getByLabelText('スキャン対象'), {
      target: { value: '/tmp/sample' },
    })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'スキャン開始' }))
    })
    expect(scanApi.startScan).toHaveBeenCalledWith('/tmp/sample')
    expect(await screen.findByText('スキャン中')).toBeInTheDocument()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250)
    })
    expect(scanApi.getScanStatus).toHaveBeenCalledWith(7)
    expect(screen.getByText('完了')).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: '/tmp/sample' })).toBeInTheDocument()
    expect(screen.getByText('1.0 KB')).toBeInTheDocument()
  })
})
