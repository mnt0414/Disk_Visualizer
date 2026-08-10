import { useEffect, useState } from 'react'
import { MockView } from './features/MockView'
import { loadAppInfo } from './services/appInfo'
import type { AppInfo } from './types/app'

type ViewId = 'overview' | 'storage-map' | 'large-items' | 'app-cache' | 'comparison' | 'scan' | 'settings'
type Theme = 'system' | 'light' | 'dark'

const views: Array<{ id: ViewId; label: string; description: string; icon: string }> = [
  { id: 'overview', label: '概要', description: 'ストレージ全体の状態', icon: '▦' },
  { id: 'storage-map', label: 'ストレージマップ', description: '容量の分布を可視化', icon: '◇' },
  { id: 'large-items', label: '大容量項目', description: 'サイズの大きい項目', icon: '▥' },
  { id: 'app-cache', label: 'アプリキャッシュ', description: '再生成可能なデータ', icon: '↻' },
  { id: 'comparison', label: '比較', description: 'スナップショットの差分', icon: '⇄' },
  { id: 'scan', label: 'スキャン', description: 'キューと進捗', icon: '◎' },
  { id: 'settings', label: '設定', description: '動作と表示の設定', icon: '⚙' },
]

function resolveTheme(theme: Theme) {
  if (theme !== 'system') return theme
  if (!window.matchMedia) return 'dark'
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'
}

function Overview() {
  const categories = [
    ['制作データ', '184 GB', 'treemap-one'],
    ['アプリケーション', '112 GB', 'treemap-two'],
    ['写真', '86 GB', 'treemap-three'],
    ['アプリキャッシュ', '64 GB', 'treemap-four'],
    ['書類', '38 GB', 'treemap-five'],
    ['その他', '26 GB', 'treemap-six'],
  ]
  return <><section className="storage-summary" aria-labelledby="storage-title"><div className="storage-copy"><span className="eyebrow">Macintosh HD</span><h2 id="storage-title">ストレージの使用状況</h2><p>510 GB使用中（合計1 TB）</p><div className="storage-meter" role="meter" aria-label="ストレージ使用量" aria-valuemin={0} aria-valuemax={1000} aria-valuenow={510}><span style={{ width: '51%' }} /></div><div className="storage-legend"><span><i className="legend-used" />使用中 510 GB</span><span><i className="legend-free" />空き 490 GB</span></div></div><div className="usage-ring" aria-hidden="true"><strong>51%</strong><span>使用中</span></div></section><section className="metric-grid" aria-label="スキャンの概要"><article className="metric-card"><span>項目数</span><strong>2,481,306</strong><small>ファイルとフォルダ</small></article><article className="metric-card"><span>アプリキャッシュ</span><strong>64.2 GB</strong><small>48か所を認識</small></article><article className="metric-card"><span>最終スキャン</span><strong>今日 09:42</strong><small>標準スキャン · 4分12秒</small></article></section><div className="content-grid"><section className="panel distribution-panel"><div className="panel-heading"><div><span className="eyebrow">容量の分布</span><h2>上位カテゴリ</h2></div></div><div className="treemap" role="img" aria-label="カテゴリ別の容量分布">{categories.map(([label,size,className]) => <div className={`treemap-tile ${className}`} key={label}><strong>{label}</strong><span>{size}</span></div>)}</div></section><section className="panel insights-panel"><div className="panel-heading"><div><span className="eyebrow">インサイト</span><h2>確認候補</h2></div></div><ul className="insight-list"><li><span className="insight-icon cache">↻</span><div><strong>アプリキャッシュ</strong><p>64.2 GB · 48か所</p></div></li><li><span className="insight-icon large">▥</span><div><strong>10 GB以上の項目</strong><p>8件 · 合計142 GB</p></div></li><li><span className="insight-icon compare">⇄</span><div><strong>前回からの増加</strong><p>制作データが18.4 GB増加</p></div></li></ul></section></div></>
}

export function App() {
  const [viewId, setViewId] = useState<ViewId>('overview')
  const [theme, setTheme] = useState<Theme>('system')
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null)
  const active = views.find((view) => view.id === viewId) ?? views[0]

  useEffect(() => { let mounted = true; void loadAppInfo().then((info) => { if (mounted) setAppInfo(info) }); return () => { mounted = false } }, [])
  useEffect(() => { const apply = () => { document.documentElement.dataset.theme = resolveTheme(theme) }; apply(); if (theme !== 'system' || !window.matchMedia) return; const media = window.matchMedia('(prefers-color-scheme: light)'); media.addEventListener?.('change', apply); return () => media.removeEventListener?.('change', apply) }, [theme])
  const cycleTheme = () => setTheme((value) => value === 'system' ? 'light' : value === 'light' ? 'dark' : 'system')

  return <div className="app-shell"><a className="skip-link" href="#main-content">メインコンテンツへ移動</a><header className="title-bar"><div className="brand"><span className="brand-mark">◉</span><strong>Disk Visualizer</strong><span className="build-info">{appInfo ? `${appInfo.platform} · ${appInfo.architecture}` : 'ローカル分析'}</span></div><div className="title-actions"><button className="search-button" type="button"><span>⌕</span><span>検索</span><kbd>⌘ K</kbd></button><button className="icon-button" type="button" aria-label={`テーマ: ${theme}。切り替える`} onClick={cycleTheme}>◐</button></div></header><aside className="sidebar"><div className="volume-card"><span className="volume-icon">◉</span><div><strong>Macintosh HD</strong><span>490 GB利用可能</span></div></div><p className="section-label">分析</p><nav aria-label="メインナビゲーション">{views.map((view) => <button key={view.id} type="button" aria-current={viewId === view.id ? 'page' : undefined} className={viewId === view.id ? 'nav-item active' : 'nav-item'} onClick={() => setViewId(view.id)}><span aria-hidden="true">{view.icon}</span><span>{view.label}</span></button>)}</nav><div className="sidebar-status"><span className="status-dot" /><div><strong>ローカルのみ</strong><span>外部送信なし</span></div></div></aside><main className="main-content" id="main-content" tabIndex={-1}><div className="page-heading"><div><span className="eyebrow">Disk Visualizer</span><h1>{active.label}</h1><p>{active.description}</p></div><button className="primary-button" type="button"><span aria-hidden="true">◎</span><span>新しいスキャン</span></button></div>{viewId === 'overview' ? <Overview /> : <MockView viewId={viewId} label={active.label} description={active.description} />}</main></div>
}
