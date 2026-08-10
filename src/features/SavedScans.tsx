import { useEffect, useState } from 'react'
import { deleteSavedScan, listSavedScans } from '../services/scan'
import type { SavedScan } from '../types/scan'

function formatBytes(bytes:number){if(!bytes)return'0 B';const units=['B','KB','MB','GB','TB'];const unit=Math.min(Math.floor(Math.log(bytes)/Math.log(1024)),units.length-1);return`${(bytes/1024**unit).toFixed(unit===0?0:1)} ${units[unit]}`}
export function SavedScans({refreshKey}:{refreshKey:number}){
  const [items,setItems]=useState<SavedScan[]>([]);const [error,setError]=useState('')
  const load=async()=>{try{setItems(await listSavedScans());setError('')}catch(reason){setError(reason instanceof Error?reason.message:String(reason))}}
  useEffect(()=>{void load()},[refreshKey])
  const remove=async(id:number)=>{try{await deleteSavedScan(id);await load()}catch(reason){setError(reason instanceof Error?reason.message:String(reason))}}
  return <section className="panel mock-panel saved-scans"><div className="mock-header"><span className="eyebrow">ローカル保存</span><h2>保存済みスキャン</h2></div>{error&&<p className="scan-error" role="alert">{error}</p>}{items.length===0?<p className="scan-empty">保存済みスキャンはありません</p>:<ul>{items.map(item=><li key={item.id}><div><strong>{item.rootPath}</strong><span>{new Date(item.completedAt*1000).toLocaleString()}・{formatBytes(item.totalSizeBytes)}・{item.fileCount.toLocaleString()}ファイル</span></div><button type="button" className="secondary-button danger-button" onClick={()=>remove(item.id)}>履歴を削除</button></li>)}</ul>}</section>
}
