import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { MockView } from './MockView'

describe('MockView', () => {
  it('renders application cache evidence', () => {
    render(<MockView viewId="app-cache" label="アプリキャッシュ" description="" />)
    expect(screen.getByRole('heading', { name: 'キャッシュ候補' })).toBeInTheDocument()
    expect(screen.getByText('Adobe Premiere Pro')).toBeInTheDocument()
  })

  it('pauses and resumes a mock scan', () => {
    render(<MockView viewId="scan" label="スキャン" description="" />)
    fireEvent.click(screen.getByRole('button', { name: '一時停止' }))
    expect(screen.getByRole('button', { name: '再開' })).toBeInTheDocument()
  })
})
