import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { chooseFolder } from '../services/folderPicker'
import { scanFolder } from '../services/scan'
import { MockView } from './MockView'

vi.mock('../services/folderPicker', () => ({ chooseFolder: vi.fn() }))
vi.mock('../services/scan', () => ({ scanFolder: vi.fn() }))
const mockedChooseFolder = vi.mocked(chooseFolder)
const mockedScanFolder = vi.mocked(scanFolder)

beforeEach(() => { mockedChooseFolder.mockReset(); mockedScanFolder.mockReset() })

describe('MockView', () => {
  it('renders application cache evidence', () => {
    render(<MockView viewId="app-cache" label="アプリキャッシュ" description="" />)
    expect(screen.getByRole('heading', { name: 'キャッシュ候補' })).toBeInTheDocument()
    expect(screen.getByText('Adobe Premiere Pro')).toBeInTheDocument()
  })

  it('opens the native folder picker', async () => {
    mockedChooseFolder.mockResolvedValue(null)
    render(<MockView viewId="scan" label="スキャン" description="" />)
    fireEvent.click(screen.getByRole('button', { name: 'フォルダを選択' }))
    await waitFor(() => expect(mockedChooseFolder).toHaveBeenCalledOnce())
  })

  it('scans a selected path and renders the result', async () => {
    mockedScanFolder.mockResolvedValue({ rootPath: '/Users/test/Documents', totalSizeBytes: 1024, fileCount: 1, directoryCount: 0, skippedCount: 0, elapsedMilliseconds: 2, entries: [{ name: 'note.txt', path: '/Users/test/Documents/note.txt', sizeBytes: 1024, fileCount: 1, directoryCount: 0, skippedCount: 0, isDirectory: false }] })
    render(<MockView viewId="scan" label="スキャン" description="" />)
    fireEvent.change(screen.getByLabelText('スキャン対象'), { target: { value: '/Users/test/Documents' } })
    fireEvent.click(screen.getByRole('button', { name: 'スキャン開始' }))
    await waitFor(() => expect(screen.getByText('note.txt')).toBeInTheDocument())
    expect(mockedScanFolder).toHaveBeenCalledWith('/Users/test/Documents')
    expect(screen.getAllByText('1.0 KB')).toHaveLength(2)
  })
})
