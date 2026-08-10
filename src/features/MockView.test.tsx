import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { scanFolder } from '../services/scan'
import { MockView } from './MockView'

vi.mock('../services/scan', () => ({ scanFolder: vi.fn() }))
const mockedScanFolder = vi.mocked(scanFolder)

beforeEach(() => mockedScanFolder.mockReset())

describe('MockView', () => {
  it('renders application cache evidence', () => {
    render(<MockView viewId="app-cache" label="アプリキャッシュ" description="" />)
    expect(screen.getByRole('heading', { name: 'キャッシュ候補' })).toBeInTheDocument()
    expect(screen.getByText('Adobe Premiere Pro')).toBeInTheDocument()
  })

  it('scans an absolute path and renders the result', async () => {
    mockedScanFolder.mockResolvedValue({ rootPath: '/Users/test/Documents', totalSizeBytes: 1024, fileCount: 1, directoryCount: 0, skippedCount: 0, elapsedMilliseconds: 2, entries: [{ name: 'note.txt', path: '/Users/test/Documents/note.txt', sizeBytes: 1024, fileCount: 1, directoryCount: 0, skippedCount: 0, isDirectory: false }] })
    render(<MockView viewId="scan" label="スキャン" description="" />)
    fireEvent.change(screen.getByLabelText('絶対パス'), { target: { value: '/Users/test/Documents' } })
    fireEvent.click(screen.getByRole('button', { name: 'スキャン開始' }))
    await waitFor(() => expect(screen.getByText('note.txt')).toBeInTheDocument())
    expect(mockedScanFolder).toHaveBeenCalledWith('/Users/test/Documents')
    expect(screen.getByText('1.0 KB')).toBeInTheDocument()
  })
})
