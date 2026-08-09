# Disk Visualizer 実装計画

## 1. 目的

macOSとWindowsで動作する、軽量・高速・完全オフラインのディスク使用量可視化ツールを実装する。

初期版では、ユーザーが容量を支配しているフォルダやアプリキャッシュを特定し、判定根拠を確認したうえで、Finderまたはエクスプローラーから安全に整理へ進める状態を完成条件とする。

アプリ内でファイルの削除・移動・内容解析は行わない。

## 2. 確定済みの前提

### 対応環境

- macOS：最新2世代、Apple Silicon `arm64`
- Windows：Windows 11、`x86_64`
- 正式対応ファイルシステム：APFS、HFS+、NTFS、exFAT
- 想定ストレージ容量：1TB未満〜20TB
- NAS・ネットワークストレージ：初期対象外

### 技術構成

- コア：Rust stable
- デスクトップランタイム：Tauri 2
- UI：React + TypeScript + Vite
- ローカルDB：SQLite
- テーマ：Studio Indigo
- ライセンス：Apache License 2.0
- 外部通信・テレメトリ：なし

### UI原則

- macOSとWindowsで共通の情報設計を使用する
- ライト、ダーク、高コントラストへ対応する
- キーボードのみで主要操作を完了できる
- 色だけで状態や容量分類を伝えない
- アプリキャッシュには判定根拠、信頼度、再生成可否、整理時の影響を表示する
- ファイル削除は行わず、OSのファイルマネージャーで開く

## 3. 初期リリースのスコープ

### 含める機能

- ドライブまたは任意フォルダの選択
- 標準スキャン、フルスキャン、差分更新
- スキャン進捗、経過時間、残り時間予測
- 一時停止、再開、キャンセル
- 複数対象の逐次スキャンキュー
- 実使用量推定値と論理サイズの集計
- ハードリンクの重複排除
- ツリーマップ
- 階層ツリー＋横棒
- 大容量順リスト
- ファイル名、パス、容量、日時、種類による検索・絞り込み
- アプリキャッシュ認識
- 保存済みスキャンデータ
- 手動スナップショット
- スナップショット比較
- CSV出力
- Finder／エクスプローラー連携
- 日本語／英語
- 診断レポートの手動生成

### 初期対象外

- アプリ内削除、移動、ゴミ箱操作
- ファイル内容の読み取り、ハッシュ比較
- 重複ファイル検出
- NAS・ネットワークストレージ
- 常駐プロセス、自動監視、自動更新確認
- macOS Intel、Windows ARM64

## 4. アーキテクチャ

### 4.1 レイヤー

```text
React UI
  ├─ 画面・コンポーネント
  ├─ UI状態管理
  ├─ 検索・フィルター状態
  └─ Tauri command client
          ↓
Tauri command boundary
  ├─ 入力検証
  ├─ 権限境界
  ├─ イベント／進捗通知
  └─ エラー変換
          ↓
Rust application core
  ├─ scan orchestration
  ├─ aggregation
  ├─ cache classification
  ├─ index trust evaluation
  ├─ snapshot comparison
  └─ export
          ↓
Infrastructure adapters
  ├─ filesystem adapter
  ├─ macOS adapter
  ├─ Windows adapter
  ├─ SQLite repository
  └─ diagnostics
```

### 4.2 Rustクレート候補

```text
src-tauri/
  src/
    app/              # ユースケースとオーケストレーション
    domain/           # 値オブジェクト、状態、分類ルール
    scanner/          # 共通走査処理
    platform/
      macos/          # FSEvents、Finder、ボリューム情報
      windows/        # USN Journal、Explorer、ボリューム情報
    storage/          # SQLite、マイグレーション
    cache_catalog/    # アプリキャッシュ定義と判定
    export/           # CSV、匿名化
    diagnostics/      # 匿名化診断情報
    commands/         # Tauri command
```

### 4.3 React構成候補

```text
src/
  app/                # ルーティング、プロバイダー、アプリシェル
  components/         # 共通UI
  features/
    dashboard/
    storage-map/
    large-items/
    app-cache/
    comparison/
    scan-queue/
    search/
    settings/
  design-system/      # Studio Indigoトークン
  services/           # Tauri command client
  stores/             # UI状態
  types/              # IPC DTO
  i18n/               # 日本語／英語
```

## 5. データモデルの初期案

### Volume

- volume_id
- display_name
- mount_path
- filesystem_type
- capacity
- available_space
- platform
- last_seen_at

### ScanSession

- scan_id
- volume_id
- root_path
- scan_mode
- load_profile
- status
- started_at
- completed_at
- total_allocated_size
- total_logical_size
- error_count
- excluded_count
- trust_state

### FileEntry

- entry_id
- scan_id
- parent_id
- file_identity
- relative_path
- entry_type
- logical_size
- allocated_size
- modified_at
- filesystem_flags
- classification

### CacheClassification

- entry_id
- definition_id
- application_name
- category
- confidence
- evidence
- regenerable
- cleanup_impact
- definition_version

### Snapshot

- snapshot_id
- volume_id
- source_scan_id
- name
- created_at

実装前のSQLite試作で、100万件、500万件、1,000万件に対する書き込み速度、インデックスサイズ、集計速度を検証して最終スキーマを決定する。

## 6. 実装フェーズ

## Phase 0：リポジトリ基盤

### 作業

- Tauri 2 + React + TypeScript + Viteの初期化
- Rust workspaceとディレクトリ構成
- ESLint、Prettier、Rustfmt、Clippy
- unit／integration／E2Eテストの雛形
- GitHub ActionsのmacOS ARM64／Windows x64ジョブ
- LICENSE、NOTICE、CONTRIBUTING、SECURITYの整備
- 依存関係ライセンス確認の仕組み

### 完了条件

- macOSとWindowsで空のアプリがビルド・起動できる
- CIでTypeScript、Rust、テスト、フォーマット検査が成功する
- デバッグビルドが外部通信を行わない

## Phase 1：UIシェルとデザインシステム

### 作業

- Studio Indigoのセマンティックトークン
- ライト、ダーク、高コントラスト
- アプリシェル、サイドバー、タイトルバー、詳細パネル
- 概要、ストレージマップ、大容量項目、アプリキャッシュ、比較、スキャン、設定
- モックデータで画面遷移を実装
- キーボード操作、フォーカス管理、Reduced Motion
- 日本語／英語の基盤

### 完了条件

- デザインプロトタイプの主要画面が実装されている
- キーボードのみで全画面へ移動できる
- テーマ切り替えでレイアウトが破綻しない
- UI単純操作が200ms以内に反応する

## Phase 2：最小スキャナー縦切り

### 作業

- フォルダ選択
- Rustによる読み取り専用の逐次走査
- ファイル／フォルダの論理サイズ取得
- キャンセルトークン
- 進捗イベント
- 集計結果をメモリからUIへ返す
- モックデータを実データへ差し替える

### 完了条件

- 任意フォルダを選択して容量上位フォルダを表示できる
- キャンセル後にUIが操作可能な状態へ戻る
- ファイル内容を読み取らず、変更も行わない
- 不正なパスやアクセス拒否でクラッシュしない

## Phase 3：ストリーミング集計とSQLite

### 作業

- SQLiteスキーマとマイグレーション
- バッチ書き込み
- ストリーミング集計
- 全件をメモリへ保持しない設計
- 保存済みスキャンデータの一覧・削除
- 不完全なスキャンを正常扱いしない
- DB整合性検査と安全な移行

### 完了条件

- 100万件の試験データをメモリ上限内で処理できる
- 中断・クラッシュ後に不完全DBを識別できる
- 既存DBを保護した状態で移行を試行できる

## Phase 4：容量精度とファイルシステム境界

### 作業

- 割り当て済みサイズ
- ハードリンク重複排除
- スパースファイル、圧縮ファイル
- シンボリックリンク、ジャンクション、リパースポイント非追跡
- 別ボリューム境界
- macOSパッケージ／バンドル表示
- 未集計・除外・変動中の明示

### 完了条件

- APFS、HFS+、NTFS、exFATの検証ケースを通過する
- 別ボリュームを自動横断しない
- 実使用量推定値と論理サイズの差を説明できる

## Phase 5：アプリキャッシュ認識

### 作業

- 署名済みアプリ更新へ同梱する定義形式
- OS、アプリ、バージョン、パス、根拠、信頼度
- 固定パス、設定検出パス、ユーザー指定パスの区別
- 使用中・変動中の状態
- 判定根拠と整理時の影響表示

### 初期カタログ

- macOS／Windows標準キャッシュ
- Chrome、Edge、Safari、Firefox
- Adobe Premiere Pro、After Effects、Media Encoder、Photoshop、Illustrator
- DaVinci Resolve
- Autodesk Flame
- Blender

### 完了条件

- 名前や日時だけでキャッシュ認定しない
- 「安全に削除できる」と断定しない
- UIから定義バージョンと判定根拠を確認できる

## Phase 6：差分更新・キュー・電源状態

### 作業

- macOS FSEvents
- Windows USN Change Journal
- 履歴連続性と信頼状態
- 部分再スキャン／フルスキャン提案
- 逐次スキャンキュー
- スリープ／復帰
- バッテリー／省電力状態
- サーマル状態による負荷制御

### 完了条件

- 履歴欠落を正常な差分結果として扱わない
- 同時スキャン数が常に1である
- スリープ復帰後に対象の同一性を再確認する
- 負荷制御で精度と対象範囲を変更しない

## Phase 7：検索・比較・出力

### 作業

- 名前、パス、容量、更新日時、拡張子、分類フィルター
- ツリーマップ／階層／リストで条件を共有
- 手動スナップショット
- スナップショット比較
- CSV相対パス／フルパス／匿名化
- CSV Formula Injection対策

### 完了条件

- 条件変更へ100ms以内に視覚的反応を返す
- フルパス出力は毎回明示選択が必要
- 匿名化CSVに元の名前やユーザー名が含まれない

## Phase 8：権限・診断・配布

### 作業

- macOS Full Disk Access案内
- Windowsの一回限りの昇格スキャナー設計
- 標準権限UIと昇格プロセスの分離
- 匿名化診断レポート
- macOS署名・Notarization・DMG
- Windowsコード署名・インストーラー
- SBOM、依存ライセンス一覧
- セキュリティ回帰テスト

### 完了条件

- 起動時に追加権限を要求しない
- 権限拒否後も読み取り可能範囲で続行できる
- 診断情報を自動送信しない
- 署名済み配布物を生成できる

## 7. 非機能要件

### 応答性

- UI視覚反応：p95 100ms以内
- 単純操作：最悪200ms以内
- メインスレッド単一タスク：50ms未満
- アニメーション処理：10ms未満／フレーム

### 起動

- コールド起動：p95 2秒以内
- ウォーム起動：p95 1秒以内

### メモリ

- 待機時目標：200MB以下
- 待機時上限：300MB
- スキャン時目標：500MB以下
- スキャン時ソフト上限：750MB
- スキャン時リリース上限：1GB

### CPU

- 待機時：安定後60秒平均0.5%未満
- 待機中の継続的I/O、ポーリング、不要なタイマー起床を禁止

### 配布サイズ

- 目標：30MB以下
- ソフト上限：50MB

## 8. テスト戦略

### Rust単体テスト

- パス境界と入力検証
- 容量集計
- ハードリンク重複排除
- キャッシュ定義判定
- 信頼状態遷移
- CSVエスケープと匿名化

### 統合テスト

- SQLiteマイグレーション
- 中断、キャンセル、クラッシュ復旧
- 権限エラー
- 外付けドライブ切断
- リンク競合と別ボリューム境界

### UIテスト

- 画面遷移
- キーボード操作
- フォーカス順
- ライト／ダーク／高コントラスト
- 文字拡大とDPI
- Reduced Motion
- 空、読み込み中、エラー、権限不足、切断状態

### E2E

- 対象選択からスキャン完了まで
- スキャン結果からFinder／エクスプローラーで開くまで
- 保存、比較、CSV出力
- アプリ終了時に常駐プロセスが残らないこと

### セキュリティ

- 悪意あるファイル名
- 制御文字、方向制御文字、異常Unicode
- 長いパスと深い階層
- シンボリックリンク／ジャンクション差し替え
- 巨大メタデータ
- CSV Formula Injection
- ログ／診断情報へのパス漏えい
- シークレットスキャン

## 9. CI計画

### Pull Request

- TypeScript typecheck
- ESLint／Prettier
- Rustfmt／Clippy
- Rust unit tests
- React unit tests
- 最小Tauriビルド
- シークレットスキャン
- 依存ライセンス検査

### main

- macOS arm64ビルド
- Windows x64ビルド
- 統合テスト
- アーティファクト生成

### Release

- 署名
- Notarization／Windows署名検証
- SBOM生成
- SHA-256生成
- LICENSE／NOTICE確認

## 10. Pull Request分割案

1. `chore/bootstrap-tauri-react`
2. `feat/design-system-app-shell`
3. `feat/mock-dashboard-navigation`
4. `feat/minimal-folder-scanner`
5. `feat/scan-progress-cancellation`
6. `feat/sqlite-index`
7. `feat/filesystem-accuracy`
8. `feat/app-cache-catalog`
9. `feat/incremental-scan`
10. `feat/snapshots-search-export`
11. `feat/permissions-diagnostics`
12. `chore/release-pipeline`

各PRは単独でレビュー・テスト可能なサイズに保ち、UI変更にはライト／ダーク両方のスクリーンショットを添付する。

## 11. 最初の実装マイルストーン

### Milestone 1：最小エンドツーエンド

以下を最初の実装完了条件とする。

1. macOS arm64とWindows x64でアプリが起動する
2. ユーザーが任意フォルダを選択できる
3. Rustが読み取り専用でフォルダを走査する
4. 進捗をReact UIへ通知する
5. 容量上位フォルダを一覧と簡易ツリーマップへ表示する
6. スキャンをキャンセルできる
7. Finder／エクスプローラーで選択対象を開ける
8. 外部通信、テレメトリ、ファイル変更を行わない

Milestone 1では、差分更新、完全なアプリキャッシュカタログ、スナップショット比較、署名済み配布は後続フェーズとする。

## 12. 未確定事項

実装と並行して以下を確定する。

- 製品の正式表示名
- Bundle ID／Windowsアプリ識別子
- 標準スキャンのOS別推奨除外リスト
- 差分更新の信頼を失う具体条件
- アプリキャッシュ定義の署名、廃止、互換性方針
- 最低基準ハードウェアとベンチマーク手順
- アプリ更新、セキュリティ告知、旧版サポート方針
- リリース受け入れテスト

## 13. Definition of Done

機能は次をすべて満たした場合に完了とする。

- 受け入れ条件を自動または手動テストで確認できる
- Rustfmt、Clippy、TypeScript、Lintが成功する
- 正常系、エラー、キャンセルのテストがある
- macOSとWindowsの差異が文書化されている
- キーボードとスクリーンリーダーの確認項目がある
- 外部通信とファイル変更を追加していない
- ログに機密パスを平文で残さない
- パフォーマンス予算への影響を記録する
- ユーザー向け文言が日本語と英語で用意されている
