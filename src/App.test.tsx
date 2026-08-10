import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { App } from './App'

describe('App', () => {
  it('renders the overview dashboard and accessible navigation', () => {
    render(<App />)
    expect(screen.getByRole('heading', { name: '概要', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('navigation', { name: 'メインナビゲーション' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '新しいスキャン' })).toBeInTheDocument()
  })
})
