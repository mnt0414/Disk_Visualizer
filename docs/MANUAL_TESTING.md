# 手動動作確認ガイド

現時点で動作確認を始めてよい。Phase 5の主要な読み取り専用フローである、フォルダー選択、スキャン、SQLite保存、保存履歴、アプリキャッシュ分類、観測状態表示まで接続されている。

アプリ内からファイルを削除・移動する機能はまだ提供していない。初回は小さなフォルダーを対象にし、表示と読み取り専用動作を確認する。

## 現在確認できること

- macOS／Windowsのネイティブフォルダー選択
- 読み取り専用スキャンと進捗表示
- 一時停止、再開、キャンセル
- 完了したスキャン履歴のSQLite保存
- ブラウザーキャッシュ候補の分類
- 論理サイズと実使用量推定の表示
- 観測状態の「変化なし」「変化を検出」「判定できず」「未記録」表示
- ライト、ダーク、高コントラスト表示

## 現在の制限

- アプリ内削除・移動は未実装
- アプリキャッシュ定義はSafari、Chrome、Edge、Firefoxが中心
- Adobe、DaVinci Resolve、Autodesk Flame、Blender定義は調査中
- 「変化なし」は未使用を意味しない
- 「変化を検出」はスキャン中に対象が実際に更新された場合だけ表示されるため、毎回再現するとは限らない
- v5以前に保存した履歴は観測状態が「未記録」になる
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
- Microsoft Edge WebView2 Runtime。通常はWindows 11へ導入済み

## 起動手順

```bash
git clone https://github.com/mnt0414/Disk_Visualizer.git
cd Disk_Visualizer
git switch main
git pull
npm install
npm run tauri dev
```

`npm run tauri dev`は必要なアイコンを生成してから、ViteとTauriの開発版を起動する。

## 最初のスモークテスト

### 1. 小さなフォルダーをスキャンする

1. アプリを起動する。
2. 「スキャン」を開く。
3. 「フォルダを選択」から、まず数百MB以下のテスト用フォルダーを選ぶ。
4. スキャンを開始し、進捗、現在のパス、件数が更新されることを確認する。
5. 完了後、ファイル数、フォルダー数、論理サイズ、実使用量推定が表示されることを確認する。

初回からホーム全体、システムドライブ全体、権限が必要な領域を選ばない。アプリの基本動作を確認してから対象を広げる。

### 2. 保存履歴を確認する

1. 「保存済みスキャン」を開く。
2. 完了したスキャンが一覧にあることを確認する。
3. アプリを再起動する。
4. 同じ履歴が残っていることを確認する。

キャンセルまたは失敗したスキャンが正常完了として表示されないことも確認する。

### 3. アプリキャッシュ表示を確認する

キャッシュ分類を確認しやすい対象例:

macOS:

- `~/Library/Caches/Google/Chrome`
- `~/Library/Caches/Firefox/Profiles`
- `~/Library/Caches/com.apple.Safari`

Windows:

- `%LOCALAPPDATA%\Google\Chrome\User Data\Default\Cache`
- `%LOCALAPPDATA%\Microsoft\Edge\User Data\Default\Cache`
- `%LOCALAPPDATA%\Mozilla\Firefox\Profiles`

存在するフォルダーを一つ選び、スキャン完了後に「アプリキャッシュ」を開く。

確認項目:

- アプリ名と分類が表示される
- パスと実使用量推定が表示される
- 信頼度、根拠、再生成可否、整理時の影響を展開できる
- 観測状態が表示される
- 「変化なし」の説明に、未使用を保証しない旨がある
- 過去形式の履歴は「未記録」と表示され、「判定できず」と区別される

ブラウザーを終了してから試すと結果が安定しやすいが、「変化なし」であっても未使用とは断定しない。

### 4. 安全性を確認する

スキャン前後で次を確認する。

- 対象ファイルが削除・移動されていない
- ファイル内容が変更されていない
- Finder／Explorerを開く案内以外に削除操作がない
- ネットワーク送信を要求する画面がない

## 任意の品質チェック

フロントエンド:

```bash
npm run format:check
npm run check
npm test
npm run build
```

Rust:

```bash
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

## 問題を報告するとき

次を記録する。

- macOS／Windowsのバージョン
- CPUアーキテクチャ
- 実行した操作
- 期待した結果と実際の結果
- エラーメッセージ
- 再現頻度
- 対象がローカル、外付け、ネットワークドライブのどれか

パスやファイル名に個人情報が含まれる場合は、スクリーンショットやログを共有する前に伏せる。

## 推奨する確認タイミング

今の段階で一度確認するのがよい。Phase 5の主要フローが接続されたため、実データ上のパス表示、サイズ感、スキャン速度、観測状態の文言について早めにフィードバックを得られる。

ただし、まだ完成版の受け入れテストではない。今回を読み取り専用の中間スモークテストとし、アプリ定義追加とPhase 5完了レビュー後に、より広い対象で再度確認する。
