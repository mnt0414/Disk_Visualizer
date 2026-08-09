import { useEffect, useState } from 'react'
import { loadAppInfo } from './services/appInfo'
import type { AppInfo } from './types/app'

const navigation = [
  '概要',
  'ストレージマップ',
  '大容量項目',
  'アプリキャッシュ',
  '比較',
  'スキャン',
  '設定',
] as const

export function App() {
  const [activeView, setActiveView] = useState<(typeof navigation)[number]>('概要')
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null)
  const [theme, setTheme] = useState<'light' | 'dark'>('dark')

  useEffect(() => {
    document.documentElement.dataset.theme = theme
  }, [theme])

  useEffect(() => {
    let active = true
    void loadAppInfo().then((info) => {
      if (active) setAppInfo(info)
    })
    return () => {
      active = false
    }
  }, [])

  return (
    <div className="app-shell">
      <header className="title-bar">
        <div>
          <strong>Disk Visualizer</strong>
          <span className="build-info">
            {appInfo ? `${appInfo.platform} · ${appInfo.architecture}` : '起動情報を確認中'}
          </span>
        </div>
        <button
          className="icon-button"
          type="button"
          aria-label={`${theme === 'dark' ? 'ライト' : 'ダーク'}テーマに切り替える`}
          onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="4" />
            <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
          </svg>
        </button>
      </header>

      <aside className="sidebar" aria-label="メインナビゲーション">
        <p className="section-label">分析</p>
        <nav>
          {navigation.map((item) => (
            <button
              key={item}
              type="button"
              aria-current={activeView === item ? 'page' : undefined}
              className={activeView === item ? 'nav-item active' : 'nav-item'}
              onClick={() => setActiveView(item)}
            >
              <span className="nav-marker" aria-hidden="true" />
              {item}
            </button>
          ))}
        </nav>
      </aside>

      <main className="main-content" tabIndex={-1}>
        <div className="page-heading">
          <div>
            <h1>{activeView}</h1>
            <p>Phase 0 · アプリ基盤と画面遷移</p>
          </div>
          <button className="primary-button" type="button">
            新しいスキャン
          </button>
        </div>

        <section className="status-grid" aria-label="アプリ基盤の状態">
          <article className="status-card">
            <span>デスクトップ基盤</span>
            <strong>Tauri 2</strong>
            <small>Rust command境界を準備済み</small>
          </article>
          <article className="status-card">
            <span>UI</span>
            <strong>React</strong>
            <small>TypeScript strict mode</small>
          </article>
          <article className="status-card">
            <span>テーマ</span>
            <strong>Studio Indigo</strong>
            <small>ライト／ダーク対応</small>
          </article>
        </section>

        <section className="empty-state">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <rect x="4" y="3" width="16" height="18" rx="3" />
            <path d="M8 16h8M8 8h.01" />
          </svg>
          <h2>{activeView}を実装する準備ができました</h2>
          <p>
            現在はPhase 0の基盤です。次のPRでStudio Indigoの完全なアプリシェルとモックデータを追加します。
          </p>
        </section>
      </main>
    </div>
  )
}
