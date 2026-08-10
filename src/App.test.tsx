import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { App } from './App'

describe('App', () => {
  it('renders the overview dashboard and accessible navigation', () => {
    render(<App />)
    expect(screen.getByRole('heading', { name: '概要', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('navigation', { name: 'メインナビゲーション' })).toBeInTheDocument()
    expect(screen.getByRole('meter', { name: 'ストレージ使用量' })).toHaveAttribute('aria-valuenow', '510')
  })

  it('navigates to application cache', () => {
    render(<App />)
    fireEvent.click(screen.getByRole('button', { name: 'アプリキャッシュ' }))
    expect(screen.getByRole('heading', { name: 'アプリキャッシュ', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'アプリキャッシュ' })).toHaveAttribute('aria-current', 'page')
  })

  it('cycles the theme preference', () => {
    render(<App />)
    const button = screen.getByRole('button', { name: /テーマ: システム設定/ })
    fireEvent.click(button)
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(screen.getByRole('button', { name: /テーマ: ライト/ })).toBeInTheDocument()
  })
})
