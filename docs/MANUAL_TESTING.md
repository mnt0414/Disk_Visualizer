# 手動動作確認ガイド

Phase 5の主要な読み取り専用フローである、フォルダー選択、スキャン、SQLite保存、保存履歴、アプリキャッシュ分類、観測状態表示まで接続されている。初回は小さなフォルダーを対象にする。

アプリ内からファイルを削除・移動する機能は提供していない。

## 現在確認できること

- macOS／Windowsのネイティブフォルダー選択
- 読み取り専用スキャンと進捗表示
- 一時停止、再開、キャンセル
- 完了したスキャン履歴のSQLite保存
- Safari、Chrome、Edge、Firefoxのキャッシュ分類
- Adobe Media Cacheの既定パス分類
- Blender Asset Library索引キャッシュの既定パス分類
- 論理サイズと実使用量推定の表示
- 観測状態の「変化なし」「変化を検出」「判定できず」「未記録」表示
- ライト、ダーク、高コントラスト表示

## 現在の制限

- アプリ内削除・移動は未実装
- DaVinci Resolve、Autodesk Flame、設定変更済みAdobeパスは設定読取未実装のため分類しない
- Blenderの物理シミュレーション、レンダー一時データ、自動保存、外部キャッシュは分類しない
- 「変化なし」は未使用を意味しない
- v5以前の履歴は観測状態が「未記録」になる
- 表示した実使用量推定と、実際に解放できる容量は一致しない場合がある

## 必要な環境

共通:

- Git
- Node.js 22
- npm
- Rust stable

macOS:

- Apple Silicon Mac
- Xcode Command Line Tools。未導入の場合は `xcode-select --install`

Windows 11:

- Visual Studio Build Toolsの「C++によるデスクトップ開発」
- Microsoft Edge WebView2 Runtime

## 起動手順

```bash
git clone https://github.com/mnt0414/Disk_Visualizer.git
cd Disk_Visualizer
git switch main
git pull
npm install
npm run tauri dev
```

## 最初のスモークテスト

### 1. 小さなフォルダーをスキャンする

1. アプリを起動する。
2. 「スキャン」を開く。
3. 「フォルダを選択」から、まず数百MB以下のテスト用フォルダーを選ぶ。
4. スキャンを開始し、進捗、現在のパス、件数が更新されることを確認する。
5. 完了後、ファイル数、フォルダー数、論理サイズ、実使用量推定が表示されることを確認する。

初回からホーム全体、システムドライブ全体、権限が必要な領域を選ばない。

### 2. 保存履歴を確認する

1. 「保存済みスキャン」を開く。
2. 完了したスキャンが一覧にあることを確認する。
3. アプリを再起動する。
4. 同じ履歴が残っていることを確認する。

キャンセルまたは失敗したスキャンが正常完了として表示されないことも確認する。

### 3. アプリキャッシュ表示を確認する

ブラウザーの例:

- macOS: `~/Library/Caches/Google/Chrome`、`~/Library/Caches/Firefox/Profiles`、`~/Library/Caches/com.apple.Safari`
- Windows: `%LOCALAPPDATA%\Google\Chrome\User Data\Default\Cache`、`%LOCALAPPDATA%\Microsoft\Edge\User Data\Default\Cache`、`%LOCALAPPDATA%\Mozilla\Firefox\Profiles`

Adobeの例:

- macOS: `~/Library/Application Support/Adobe/Common/Media Cache Files`
- Windows: `%APPDATA%\Adobe\Common\Media Cache Files`

Blenderの例:

- macOS: `/Library/Caches/Blender`
- Windows: `%LOCALAPPDATA%\Blender Foundation\Blender\Cache`

実在するフォルダーだけを選び、アプリを終了してから試す。Adobeは可能であれば先にアプリ内のキャッシュ管理機能を優先する。

確認項目:

- アプリ名、分類、パス、実使用量推定が表示される
- 信頼度、根拠、再生成可否、整理時の影響を展開できる
- 観測状態が表示される
- 「変化なし」が未使用を保証しない旨が表示される
- 過去形式の履歴は「未記録」と表示される

### 4. 安全性を確認する

- 対象ファイルが削除・移動されていない
- ファイル内容が変更されていない
- Finder／Explorerを開く案内以外に削除操作がない
- ネットワーク送信を要求する画面がない

## 任意の品質チェック

```bash
npm run format:check
npm run check
npm test
npm run build
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## 問題を報告するとき

macOS／Windowsのバージョン、CPU、操作、期待結果と実際の結果、エラー、再現頻度、対象ストレージ種別を記録する。個人情報を含むパスやファイル名は共有前に伏せる。

## 推奨する確認タイミング

Phase 5完了時点の中間スモークテストとして実施する。完成版の受け入れテストではないため、最初は小さな読み取り専用対象から確認する。
