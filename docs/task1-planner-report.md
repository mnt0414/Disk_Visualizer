# タスク1：差分更新plannerの境界修正

## 状態

2026-09-07。plannerの境界修正をMacローカルへ適用し、Rust検証を実施した。
タスク1「共通差分更新・macOS統合」全体は未完了。
今回変更のWindows検証、GitHub CI、アプリの実機受け入れ検証は未実施。

## コード基準

- 元のmain：218824046a3df909467df387b90fe29b049004e4
- 依存修復のローカルコミット：ec6323a
- 修正対象：src-tauri/src/incremental_rescan.rs
- 元ソースのblob：996ad756e1488ad1fb64b2ada3d2cd2b531e0366
- パッチ適用前に、基準HEAD・元ソースblob・配布パッチSHA-256を確認した。

## 症状・原因・修正

1. 非再帰の親targetによって子targetが除去され、子の変更を取りこぼす計画になっていた。
   祖先への統合をrecursiveの場合だけに限定し、exactでは子targetを保持する。
2. root変更を検出すると早期returnし、後続の不正pathを検査していなかった。
   rootの存在を記録して全変更のpath検証を継続し、その後に計画を確定する。
3. root targetだけがmax_targets=0を迂回していた。
   空計画以外は上限0で拒否し、TooManyTargetsを返す。

## 回帰テスト

既存5本を保持し、8本を追加。ソース上のテスト定義は計13本。
exact親子の保持、target上限、root前後の不正path、空計画、
入力順序、文字列prefixと親子関係の区別、変更範囲の被覆を検証する。

## Macローカル検証

環境：macOS 15.4／arm64、Rust 1.97.1、Cargo 1.97.1。

以下を順次実行するスクリプトが終了コード0で完了した。
結果は2026-09-07 22:43 JSTの実行者報告に基づく。

- 修正ファイルのRustfmt整形
- cargo fmt --check --manifest-path src-tauri/Cargo.toml
- cargo test --locked --manifest-path src-tauri/Cargo.toml incremental_rescan::tests
- cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
- cargo test --locked --manifest-path src-tauri/Cargo.toml
- git diff --check

ログ：~/Library/Logs/Disk_Visualizer/task1-planner-7S2Ond/check.log

チャット上では個別テスト件数・詳細ログ全文までは照合していない。
MacのRustテスト成功を、差分更新機能全体の受け入れ成功とは扱わない。

## 影響と制約

- スキーマ、依存、UI、OS API、公開関数シグネチャは変更しない。
- exact targetを保持した結果、上限超過でフルスキャンへ戻る場合がある。
- ファイル内容の読み取り、削除、外部送信は追加しない。
- Windows形式prefix／NUL検証や入力総量のハード上限は今回変更していない。
- tsconfig.app.tsbuildinfoとtsconfig.node.tsbuildinfoは今回のコミット対象外。

## タスク1の残作業

- device-relativeからscan-root-relativeへの安全な変換
- capability-based部分scannerと、消失／アクセス拒否／読取失敗の区別
- rename・削除・directoryとfileの型変更に必要な再帰範囲の確定
- 有界なSQLite stagingとストリーミング適用
- baselineとcheckpointの対応、identity再確認、失敗・キャンセル時の維持
- 未変更領域を含むハードリンク再集計
- ScanManager接続、信頼状態とフルスキャン理由の返却
- 変更停止後の新規フルスキャンとの一致、旧履歴保持、メモリ上限の検証
- Windowsでの回帰検証

タスク2以降は未着手。初期リリースの機能範囲は削減していない。
