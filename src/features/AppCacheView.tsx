import { useEffect, useMemo, useRef, useState } from 'react'
import { listCacheEntries, listSavedScans } from '../services/scan'
import type { CacheEntryDetail, SavedScan } from '../types/scan'

const evidenceLabels: Record<string, string> = {
  knownApplicationCachePath: '既知のアプリキャッシュパス',
  platformSpecificPath: 'OS固有の保存場所',
}
const impactLabels: Record<string, string> = {
  browserMayRebuildCacheAndLoadSlower:
    '次回起動後に再生成され、一時的に読み込みが遅くなる場合があります',
}
const categoryLabels = {
  browser: 'ブラウザ',
  media: 'メディア',
  operatingSystem: 'OS',
}
const confidenceLabels = { high: '高', medium: '中', low: '低' }

function formatBytes(bytes: number) {
  if (!bytes) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const unit = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1,
  )
  return `${(bytes / 1024 ** unit).toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`
}
function describeScan(scan: SavedScan) {
  return `${new Date(scan.completedAt * 1000).toLocaleString()} · ${scan.rootPath}`
}
function displaySize(entry: CacheEntryDetail) {
  return entry.allocatedSize ?? entry.sizeBytes
}

export function AppCacheView() {
  const [scans, setScans] = useState<SavedScan[]>([])
  const [selectedScanId, setSelectedScanId] = useState<number | null>(null)
  const [entries, setEntries] = useState<CacheEntryDetail[]>([])
  const [loadingScans, setLoadingScans] = useState(true)
  const [loadingEntries, setLoadingEntries] = useState(false)
  const [error, setError] = useState('')
  const generation = useRef(0)

  useEffect(() => {
    const current = ++generation.current
    setLoadingScans(true)
    void listSavedScans()
      .then((items) => {
        if (current !== generation.current) return
        setScans(items)
        setSelectedScanId(items[0]?.id ?? null)
        setError('')
      })
      .catch((reason) => {
        if (current === generation.current)
          setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (current === generation.current) setLoadingScans(false)
      })
    return () => {
      generation.current += 1
    }
  }, [])

  useEffect(() => {
    if (selectedScanId === null) {
      setEntries([])
      return
    }
    const current = ++generation.current
    setLoadingEntries(true)
    void listCacheEntries(selectedScanId, 100, 0)
      .then((items) => {
        if (current === generation.current) {
          setEntries(items)
          setError('')
        }
      })
      .catch((reason) => {
        if (current === generation.current)
          setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (current === generation.current) setLoadingEntries(false)
      })
  }, [selectedScanId])

  const totalBytes = useMemo(
    () => entries.reduce((total, entry) => total + displaySize(entry), 0),
    [entries],
  )

  return (
    <div className="mock-stack app-cache-view">
      <section className="cache-toolbar" aria-label="キャッシュ履歴の選択">
        <div>
          <span className="eyebrow">ローカル保存</span>
          <strong>保存済みスキャンから表示</strong>
        </div>
        <label>
          <span>スキャン履歴</span>
          <select
            value={selectedScanId ?? ''}
            disabled={loadingScans || scans.length === 0}
            onChange={(event) => setSelectedScanId(Number(event.target.value))}
          >
            {scans.map((scan) => (
              <option key={scan.id} value={scan.id}>
                {describeScan(scan)}
              </option>
            ))}
          </select>
        </label>
      </section>
      {error && (
        <p className="scan-error" role="alert">
          {error}
        </p>
      )}
      <div className="cache-callout" aria-live="polite">
        <strong>{formatBytes(totalBytes)}</strong>
        <span>
          {loadingEntries
            ? 'キャッシュ候補を読み込んでいます…'
            : `${entries.length.toLocaleString()}件のアプリキャッシュ候補`}
        </span>
      </div>
      <section className="panel mock-panel cache-results">
        <div className="mock-header">
          <span className="eyebrow">スキャン結果</span>
          <h2>キャッシュ候補</h2>
        </div>
        {loadingScans || loadingEntries ? (
          <p className="scan-empty">キャッシュ候補を読み込んでいます…</p>
        ) : scans.length === 0 ? (
          <p className="scan-empty">
            保存済みスキャンがありません。スキャンを完了すると候補を確認できます。
          </p>
        ) : entries.length === 0 ? (
          <p className="scan-empty">このスキャンにキャッシュ候補はありません</p>
        ) : (
          <div className="cache-table-scroll">
            <table>
              <thead>
                <tr>
                  <th>アプリ／分類</th>
                  <th>場所</th>
                  <th>実使用量推定</th>
                  <th>信頼度</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((entry) => {
                  const definition = entry.definition
                  return (
                    <tr key={entry.id}>
                      <td>
                        <strong>
                          {definition?.applicationName ??
                            entry.cacheDefinitionId}
                        </strong>
                        <span className="cache-meta">
                          {definition
                            ? categoryLabels[definition.category]
                            : '保存済み定義'}
                        </span>
                        <details className="cache-details">
                          <summary>判定根拠と影響</summary>
                          {definition ? (
                            <dl>
                              <div>
                                <dt>根拠</dt>
                                <dd>
                                  {definition.evidence
                                    .map(
                                      (value) => evidenceLabels[value] ?? value,
                                    )
                                    .join('、')}
                                </dd>
                              </div>
                              <div>
                                <dt>再生成</dt>
                                <dd>
                                  {definition.regenerable
                                    ? '再生成可能'
                                    : '再生成可否は未確認'}
                                </dd>
                              </div>
                              <div>
                                <dt>整理時の影響</dt>
                                <dd>
                                  {impactLabels[definition.cleanupImpact] ??
                                    definition.cleanupImpact}
                                </dd>
                              </div>
                              <div>
                                <dt>定義</dt>
                                <dd>
                                  {entry.cacheCatalogVersion} / v
                                  {entry.cacheDefinitionVersion}
                                </dd>
                              </div>
                            </dl>
                          ) : (
                            <p>
                              保存時と現在のカタログ版が異なるため、定義詳細を読み替えずに表示しています。
                            </p>
                          )}
                        </details>
                      </td>
                      <td>
                        <code title={entry.path}>{entry.path}</code>
                      </td>
                      <td>{formatBytes(displaySize(entry))}</td>
                      <td>
                        {definition
                          ? confidenceLabels[definition.confidence]
                          : '未解決'}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>
      <p className="safety-note">
        「実使用量推定」は実際に解放できる容量を保証しません。アプリ内では削除せず、Finder／Explorerで場所と影響を確認します。
      </p>
    </div>
  )
}
