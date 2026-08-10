import { useEffect, useMemo, useState } from 'react'
import { loadAppInfo } from './services/appInfo'
import type { AppInfo } from './types/app'

type ViewId =
  | 'overview'
  | 'storage-map'
  | 'large-items'
  | 'app-cache'
  | 'comparison'
  | 'scan'
  | 'settings'

type ThemePreference = 'system' | 'light' | 'dark'

type NavigationItem = {
  id: ViewId
  label: string
  description: string
  icon: IconName
}

type IconName =
  | 'overview'
  | 'map'
  | 'large'
  | 'cache'
  | 'compare'
  | 'scan'
  | 'settings'
  | 'search'
  | 'theme'
  | 'disk'
  | 'arrow'

const navigation: NavigationItem[] = [
  { id: 'overview', label: '概要', description: 'ストレージ全体の状態', icon: 'overview' },
  { id: 'storage-map', label: 'ストレージマップ', description: '容量の分布を可視化', icon: 'map' },
  { id: 'large-items', label: '大容量項目', description: 'サイズの大きい項目', icon: 'large' },
  { id: 'app-cache', label: 'アプリキャッシュ', description: '再生成可能なデータ', icon: 'cache' },
  { id: 'comparison', label: '比較', description: 'スナップショットの差分', icon: 'compare' },
  { id: 'scan', label: 'スキャン', description: 'キューと進捗', icon: 'scan' },
  { id: 'settings', label: '設定', description: '動作と表示の設定', icon: 'settings' },
]

const themeLabels: Record<ThemePreference, string> = {
  system: 'システム設定',
  light: 'ライト',
  dark: 'ダーク',
}

function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  const paths: Record<IconName, React.ReactNode> = {
    overview: <><rect x="3" y="3" width="7" height="7" rx="1" /><rect x="14" y="3" width="7" height="7" rx="1" /><rect x="3" y="14" width="7" height="7" rx="1" /><rect x="14" y="14" width="7" height="7" rx="1" /></>,
    map: <><path d="M3 6.5 8.5 3l7 3 5.5-3v14.5L15.5 21l-7-3L3 21Z" /><path d="M8.5 3v15M15.5 6v15" /></>,
    large: <><path d="M4 19V9M10 19V5M16 19V12M22 19H2" /></>,
    cache: <><path d="M20 12a8 8 0 1 1-2.34-5.66" /><path d="M20 4v6h-6" /></>,
    compare: <><path d="m8 7-4 4 4 4M4 11h12M16 17l4-4-4-4M20 13H8" /></>,
    scan: <><path d="M4 7V4h3M17 4h3v3M20 17v3h-3M7 20H4v-3" /><circle cx="12" cy="12" r="4" /></>,
    settings: <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-2.83 2.83-.06-.06A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 .6 1.7 1.7 0 0 0-.4 1.1V21h-4v-.09A1.7 1.7 0 0 0 8.6 19.4a1.7 1.7 0 0 0-1.88.34l-.06.06-2.83-2.83.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-.6-1 1.7 1.7 0 0 0-1.1-.4H3v-4h.09A1.7 1.7 0 0 0 4.6 8.6a1.7 1.7 0 0 0-.34-1.88l-.06-.06 2.83-2.83.06.06A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-.6 1.7 1.7 0 0 0 .4-1.1V3h4v.09A1.7 1.7 0 0 0 15.4 4.6a1.7 1.7 0 0 0 1.88-.34l.06-.06 2.83 2.83-.06.06A1.7 1.7 0 0 0 19.4 9c.18.37.48.72.6 1 .12.3.1.7.1 1.1h.9v4h-.09A1.7 1.7 0 0 0 19.4 15Z" /></>,
    search: <><circle cx="11" cy="11" r="7" /><path d="m20 20-4-4" /></>,
    theme: <><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></>,
    disk: <><rect x="4" y="3" width="16" height="18" rx="3" /><path d="M8 16h8M8 8h.01" /></>,
    arrow: <><path d="m9 18 6-6-6-6" /></>,
  }

  return <svg className="icon" width={size} height={size} viewBox="0 0 24 24" aria-hidden="true">{paths[name]}</svg>
}

function resolveTheme(preference: ThemePreference) {
  if (preference !== 'system') return preference
  if (typeof window === 'undefined' || !window.matchMedia) return 'dark'
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'
}

function OverviewView() {
  const treemap = [
    ['制作データ', '184 GB', 'treemap-one'],
    ['アプリケーション', '112 GB', 'treemap-two'],
    ['写真', '86 GB', 'treemap-three'],
    ['アプリキャッシュ', '64 GB', 'treemap-four'],
    ['書類', '38 GB', 'treemap-five'],
    ['その他', '26 GB', 'treemap-six'],
  ]

  return (
    <>
      <section className="storage-summary" aria-labelledby="storage-title">
        <div className="storage-copy">
          <span className="eyebrow">Macintosh HD</span>
          <h2 id="storage-title">ストレージの使用状況</h2>
          <p>510 GB使用中（合計1 TB）</p>
          <div className="storage-meter" role="meter" aria-label="ストレージ使用量" aria-valuemin={0} aria-valuemax={1000} aria-valuenow={510}>
            <span style={{ width: '51%' }} />
          </div>
          <div className="storage-legend">
            <span><i className="legend-used" />使用中 510 GB</span>
            <span><i className="legend-free" />空き 490 GB</span>
          </div>
        </div>
        <div className="usage-ring" aria-hidden="true"><strong>51%</strong><span>使用中</span></div>
      </section>

      <section className="metric-grid" aria-label="スキャンの概要">
        <article className="metric-card"><span>項目数</span><strong>2,481,306</strong><small>ファイルとフォルダ</small></article>
        <article className="metric-card"><span>アプリキャッシュ</span><strong>64.2 GB</strong><small>48か所を認識</small></article>
        <article className="metric-card"><span>最終スキャン</span><strong>今日 09:42</strong><small>標準スキャン · 4分12秒</small></article>
      </section>

      <div className="content-grid">
        <section className="panel distribution-panel" aria-labelledby="distribution-title">
          <div className="panel-heading"><div><span className="eyebrow">容量の分布</span><h2 id="distribution-title">上位カテゴリ</h2></div><button type="button" className="text-button">マップを開く <Icon name="arrow" size={15} /></button></div>
          <div className="treemap" role="img" aria-label="制作データ184 GB、アプリケーション112 GB、写真86 GB、アプリキャッシュ64 GB、書類38 GB、その他26 GB">
            {treemap.map(([label, size, className]) => <div className={`treemap-tile ${className}`} key={label}><strong>{label}</strong><span>{size}</span></div>)}
          </div>
        </section>

        <section className="panel insights-panel" aria-labelledby="insights-title">
          <div className="panel-heading"><div><span className="eyebrow">インサイト</span><h2 id="insights-title">確認候補</h2></div></div>
          <ul className="insight-list">
            <li><span className="insight-icon cache"><Icon name="cache" /></span><div><strong>アプリキャッシュ</strong><p>64.2 GB · 48か所</p></div><Icon name="arrow" size={16} /></li>
            <li><span className="insight-icon large"><Icon name="large" /></span><div><strong>10 GB以上の項目</strong><p>8件 · 合計142 GB</p></div><Icon name="arrow" size={16} /></li>
            <li><span className="insight-icon compare"><Icon name="compare" /></span><div><strong>前回からの増加</strong><p>制作データが18.4 GB増加</p></div><Icon name="arrow" size={16} /></li>
          </ul>
        </section>
      </div>
    </>
  )
}

function PlaceholderView({ item }: { item: NavigationItem }) {
  return (
    <section className="panel placeholder-view">
      <span className="placeholder-icon"><Icon name={item.icon} size={28} /></span>
      <span className="eyebrow">Phase 1</span>
      <h2>{item.label}</h2>
      <p>{item.description}を表示するための画面シェルです。詳細コンポーネントとモックデータは次の実装単位で追加します。</p>
      <div className="skeleton-grid" aria-hidden="true"><span /><span /><span /></div>
    </section>
  )
}

export function App() {
  const [activeView, setActiveView] = useState<ViewId>('overview')
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null)
  const [themePreference, setThemePreference] = useState<ThemePreference>('system')
  const activeItem = useMemo(() => navigation.find((item) => item.id === activeView) ?? navigation[0], [activeView])

  useEffect(() => {
    let active = true
    void loadAppInfo().then((info) => { if (active) setAppInfo(info) })
    return () => { active = false }
  }, [])

  useEffect(() => {
    const applyTheme = () => { document.documentElement.dataset.theme = resolveTheme(themePreference) }
    applyTheme()
    if (!window.matchMedia || themePreference !== 'system') return
    const media = window.matchMedia('(prefers-color-scheme: light)')
    media.addEventListener?.('change', applyTheme)
    return () => media.removeEventListener?.('change', applyTheme)
  }, [themePreference])

  const cycleTheme = () => setThemePreference((current) => current === 'system' ? 'light' : current === 'light' ? 'dark' : 'system')

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">メインコンテンツへ移動</a>
      <header className="title-bar">
        <div className="brand"><span className="brand-mark"><Icon name="disk" size={17} /></span><strong>Disk Visualizer</strong><span className="build-info">{appInfo ? `${appInfo.platform} · ${appInfo.architecture}` : 'ローカル分析'}</span></div>
        <div className="title-actions">
          <button className="search-button" type="button"><Icon name="search" /><span>検索</span><kbd>⌘ K</kbd></button>
          <button className="icon-button" type="button" aria-label={`テーマ: ${themeLabels[themePreference]}。切り替える`} title={`テーマ: ${themeLabels[themePreference]}`} onClick={cycleTheme}><Icon name="theme" /></button>
        </div>
      </header>

      <aside className="sidebar">
        <div className="volume-card"><span className="volume-icon"><Icon name="disk" /></span><div><strong>Macintosh HD</strong><span>490 GB利用可能</span></div></div>
        <p className="section-label">分析</p>
        <nav aria-label="メインナビゲーション">
          {navigation.map((item) => (
            <button key={item.id} type="button" aria-current={activeView === item.id ? 'page' : undefined} className={activeView === item.id ? 'nav-item active' : 'nav-item'} onClick={() => setActiveView(item.id)}>
              <Icon name={item.icon} /><span>{item.label}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-status"><span className="status-dot" /><div><strong>ローカルのみ</strong><span>外部送信なし</span></div></div>
      </aside>

      <main className="main-content" id="main-content" tabIndex={-1}>
        <div className="page-heading"><div><span className="eyebrow">Disk Visualizer</span><h1>{activeItem.label}</h1><p>{activeItem.description}</p></div><button className="primary-button" type="button"><Icon name="scan" /><span>新しいスキャン</span></button></div>
        {activeView === 'overview' ? <OverviewView /> : <PlaceholderView item={activeItem} />}
      </main>
    </div>
  )
}
