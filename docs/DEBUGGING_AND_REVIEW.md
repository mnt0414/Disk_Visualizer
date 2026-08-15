# デバッグ・コードレビューで得た知見

## 1. CI／ビルド

### Tauri icon不足

症状:

- macOS/Linux: `failed to open icon .../src-tauri/icons/icon.png`
- Windows: ``icons/icon.ico` not found``

対応:

- Tauri buildが必要とするPNG／ICOをCI前に生成する。
- 生成元はversion controlし、生成処理は`npm run icons`へ集約する。
- frontend checkだけでなくdesktop build jobでも生成stepを保証する。

知見:

- Web UIのbuild成功はdesktop bundle成功を意味しない。
- platform-specific packaging resourceはmatrix jobごとに検証する。

### npm installの長時間停止

症状:

- frontend jobが`npm install`で約35〜45分進まない。

対応:

- lockfileを使った決定的installへ寄せる。
- cache、timeout、不要な再試行を見直す。
- 修正後は同stepが秒単位で完了することを確認する。

知見:

- 「実行中」と「progressしている」を分けて監視する。
- dependency installには上限時間を設ける。

### 一時Workflow

- GitHub Actions botによるpushでは通常CIが再実行されない場合があった。
- 一時Workflowでformat/implementationを行う場合、成果物commit後に必ず削除する。
- 削除commitを通常userでpushし、その最終headに対する6 CIを確認する。
- workflow YAMLは作成前に構文を最小化し、複雑なhere-documentを避ける。

## 2. Rust／OS API

### Windows unstable API

症状:

- `use of unstable library feature 'windows_by_handle'`

知見:

- Rust standard libraryのplatform extensionはstable availabilityを確認する。
- 必要なmetadataはWindows API adapterへ隔離し、stable Rust + windows crateで取得する。

### Windows FFI field型

症状:

- `AllocationSize.QuadPart`へアクセスしたが、binding上は`i64`でcompile error。

修正:

```rust
u64::try_from(standard.AllocationSize).ok()
```

知見:

- C headerのunion/struct表現と生成済みRust bindingは一致するとは限らない。
- unsafe codeを書く前に、使用中crate/versionの型をnative CIで確認する。
- signed -> unsigned変換はchecked conversionを使う。

### handle snapshotとTOCTOU

レビューで、pathベースのmetadata取得を複数回行うと、走査中のfile置換によりsizeとidentityが別object由来になる可能性が判明した。

対応:

- Windowsでは同じfile handleからsize、allocation、file ID、volume identity、attributesをまとめて取得する。
- 取得失敗は曖昧に補完せず、状態またはskip reasonへ反映する。

### macOS canonical path

症状:

- test fileを`/var/...`として作成したが、runtimeでは`/private/var/...`へ正規化されtestが失敗。

対応:

- test expectationに`canonicalize()`を使用する。

知見:

- macOSの一時pathやsystem pathでは表示pathとcanonical pathが異なる。
- path文字列をidentityとして使わない。

## 3. scanner設計

### bounded memory

Phase 3レビューで、走査stack、完了result、UI表示が件数に比例して増えるリスクを検出した。

対応:

- SQLiteへstreaming batch writeする。
- UIへ返す上位項目数を有界化する。
- 完了resultへ全entryを詰め込まない。

### hard-link accounting

問題:

- 集計totalではhard linkを重複排除したが、保存entryとの意味が一致しない可能性があった。

対応:

- 個別entryのlogical sizeは元値を保持する。
- aggregate totalではvolume ID + file IDで重複排除する。
- UI/APIで「entry size」と「deduplicated total」を混同しない。

### link／volume boundary

- symlinkとreparse pointをfollowしない。
- rootと異なるvolume IDのdirectoryへ降りない。
- ID取得不能時は推測で除外せず、読み取り可能範囲を走査し制限を記録する。
- skipped itemとreasonをSQLiteへ保存する。

## 4. SQLite migration

### migration前backup

- file copyだけではWALを含む一貫性を保証できない。
- `VACUUM INTO`で整合backupを作り、`quick_check`で検証する。

### path backfill

- schema追加だけでなく、旧rowのpath columnを意味のある値へbackfillする必要があった。
- migration testは`user_version`だけでなくrow contentを確認する。

### 新規DBと移行DBの不一致

Phase 5で、v4 -> v5 migrationには`scan_entries_scan_cache` indexがある一方、fresh v5 creationに同indexがない不一致をレビューで検出した。

対応:

- fresh schemaにも同indexを追加。
- schema testはfresh DBと各migration routeのindexを比較する。

### 再現性

- cache classificationはcatalog version、definition ID、definition versionをrowへ保存する。
- 古いrowを現在のcatalogで暗黙に再分類しない。
- definition更新時は履歴の意味を保持する。

## 5. 状態管理と非同期処理

- scan statusをstringではなくenumで表現し、不正遷移を拒否する。
- progress pollingの一時IPC errorは即時fatalにせず復旧可能にする。
- 保存済み履歴の再読込と進行中scanのstate updateが競合しないよう、request/session identityを照合する。
- cancel後はUI操作可能状態へ戻し、incomplete sessionをcompleted扱いしない。

## 6. アプリキャッシュ分類

- 名前、更新日時、曖昧なsubstringだけでcacheと判定しない。
- path component境界で一致させ、`Cache`と`CacheBackup`のようなprefix collisionを拒否する。
- Windowsはcase-insensitive、macOSはcase-sensitive volumeの可能性を残す。
- 複数rule一致時はより具体的なpathを優先する。
- 環境変数由来rootは解決後の絶対pathを検証する。
- UIでは「削除して安全」と断定せず、根拠、信頼度、再生成可否、整理時影響を示す。

## 7. レビューの優先順位

修正は次の順で行う。

1. security／data loss／root外走査
2. correctness（identity、size、migration、state）
3. resource bound（memory、CPU、I/O、timeout）
4. recovery／diagnostics
5. maintainability／naming／format

レビュー時は最低限次を確認する。

- platform-specific codeが両native CIでcompile/testされている
- unsafe/FFIのtypeとerror pathが確認されている
- path normalizationがsecurity boundaryを弱めていない
- DB migrationがbackup、rollback方針、content testを持つ
- errorを0 byteやsuccessへ変換していない
- temporary workflowやdebug artifactが残っていない

## 8. 既知の次リスク

- APFS clone/snapshotを含む「解放可能容量」の説明
- 走査中に変動するcache directoryの判定
- FSEvents／USN Change Journalの履歴連続性
- long path、Unicode normalization、方向制御文字
- Full Disk Access／UAC使用時の権限境界
- クリエイティブアプリのcache locationがversion/configurationで変わること
- 100万〜1000万件規模の性能・DB index・UI lazy loading
