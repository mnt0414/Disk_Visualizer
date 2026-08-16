import { AppCacheView } from './AppCacheView'
import { AsyncScanView } from './AsyncScanView'
import './mock-views.css'
import './scanner.css'

type Props = { viewId: string; label: string; description: string }
const largeItems = [
  ['Premiere Projects', '~/Movies/Premiere Projects', '68.4 GB'],
  ['DaVinci CacheClip', '~/Movies/DaVinci Resolve', '41.7 GB'],
  ['Photos Library.photoslibrary', '~/Pictures', '32.6 GB'],
  ['Xcode.app', '/Applications', '18.2 GB'],
]
function DataTable({
  title,
  rows,
  eyebrow = 'モックデータ',
}: {
  title: string
  rows: string[][]
  eyebrow?: string
}) {
  return (
    <section className="panel mock-panel">
      <div className="mock-header">
        <span className="eyebrow">{eyebrow}</span>
        <h2>{title}</h2>
      </div>
      {rows.length === 0 ? (
        <p className="scan-empty">表示できる項目はありません</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>名前</th>
              <th>分類／場所</th>
              <th>サイズ</th>
              <th>状態</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={`${row[0]}-${row[1]}`}>
                <td>
                  <strong>{row[0]}</strong>
                </td>
                <td>{row[1]}</td>
                <td>{row[2]}</td>
                <td>{row[3] ?? '確認済み'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}
function StorageMap() {
  return (
    <div className="mock-layout">
      <section className="panel mock-map" aria-label="容量の分布">
        <div className="map-a">
          制作データ
          <br />
          184 GB
        </div>
        <div className="map-b">
          アプリ
          <br />
          112 GB
        </div>
        <div className="map-c">
          写真
          <br />
          86 GB
        </div>
        <div className="map-d">
          アプリキャッシュ
          <br />
          64 GB
        </div>
      </section>
      <section className="panel mock-panel">
        <div className="mock-header">
          <span className="eyebrow">選択中</span>
          <h2>Macintosh HD</h2>
        </div>
        <ul className="mock-list">
          {[
            '制作データ 184 GB',
            'アプリケーション 112 GB',
            '写真 86 GB',
            'アプリキャッシュ 64 GB',
          ].map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      </section>
    </div>
  )
}
function Comparison() {
  return (
    <div className="mock-stack">
      <section className="mock-metrics">
        <article>
          <span>使用量の変化</span>
          <strong>+22.3 GB</strong>
        </article>
        <article>
          <span>増加した項目</span>
          <strong>1,284</strong>
        </article>
        <article>
          <span>減少した項目</span>
          <strong>426</strong>
        </article>
      </section>
      <DataTable
        title="カテゴリ別の変化"
        rows={[
          ['制作データ', '7日間', '+18.4 GB', '増加'],
          ['アプリキャッシュ', '7日間', '+6.8 GB', '増加'],
          ['ダウンロード', '7日間', '-4.2 GB', '減少'],
        ]}
      />
    </div>
  )
}
function Settings() {
  return (
    <section className="panel mock-panel">
      <div className="mock-header">
        <span className="eyebrow">設定</span>
        <h2>既定の動作</h2>
      </div>
      <label className="setting">
        <span>
          <strong>初期負荷プロファイル</strong>
          <small>CPUとI/Oの使用量</small>
        </span>
        <select defaultValue="balanced">
          <option value="balanced">バランス</option>
          <option value="quiet">低負荷</option>
        </select>
      </label>
      <label className="setting">
        <span>
          <strong>ユーザー指定の除外を維持</strong>
          <small>フルスキャンでも適用</small>
        </span>
        <input type="checkbox" defaultChecked />
      </label>
      <div className="setting">
        <span>
          <strong>外部通信</strong>
          <small>スキャン結果を送信しません</small>
        </span>
        <b>無効</b>
      </div>
    </section>
  )
}
export function MockView({ viewId, label, description }: Props) {
  if (viewId === 'storage-map') return <StorageMap />
  if (viewId === 'large-items')
    return <DataTable title="サイズの大きい項目" rows={largeItems} />
  if (viewId === 'app-cache') return <AppCacheView />
  if (viewId === 'comparison') return <Comparison />
  if (viewId === 'scan') return <AsyncScanView />
  if (viewId === 'settings') return <Settings />
  return (
    <section className="panel mock-panel">
      <h2>{label}</h2>
      <p>{description}</p>
    </section>
  )
}
