# Phase 5 progress

Phase 5「アプリキャッシュ認識」は、2026-08-17に初期リリース範囲の完了レビューを通過した。詳細は[`PHASE5_REVIEW.md`](PHASE5_REVIEW.md)を参照する。

- [x] バージョン付きアプリキャッシュ定義形式
- [x] 署名済みアプリ更新へ同梱可能なJSONカタログ
- [x] OS、アプリ、バージョン制約、パス、根拠、信頼度、再生成可否、整理時影響のモデル
- [x] 固定パス、設定検出パス、ユーザー指定パスの区別
- [x] パス要素単位の境界一致とWindowsの大文字・小文字差の吸収
- [x] 初期ブラウザー定義（Safari、Chrome、Edge、Firefox）
- [x] スキャン結果へのキャッシュ分類統合
- [x] 分類定義ID・定義バージョン・カタログバージョンをSQLite v5へ保存
- [x] 保存済みキャッシュ分類のSQLite query／Tauri command
- [x] 使用中・変動中の状態判定
  - [x] `stable`／`changing`／`unknown`の観測モデルと単体テスト
  - [x] 読み取り専用のスキャン前後観測、SQLite v6保存、UI表示
  - [x] OS固有の「使用中」断定は初期リリースに導入しないと決定
- [x] UIで定義バージョン、根拠、信頼度、整理時影響を表示
- [x] 検証済み追加アプリ定義
  - [x] Adobe Media CacheのmacOS／Windows既定パス
  - [x] Blender Asset Library CacheのmacOS／Windows既定パス
- [ ] 設定読取を必要とする将来拡張
  - [ ] DaVinci ResolveのCache／Proxy／Galleryパス
  - [ ] Autodesk FlameのProject Home／Media Cacheパス
  - [ ] Adobeのユーザー指定Media Cacheパス

## 使用中・変動中判定

別プロセスがファイルを開いていることをmacOS／Windows共通の意味で断定しない。スキャン前後に取得したfile identity、論理サイズ、更新時刻を比較し、変化を観測した場合だけ`changing`、同一なら`stable`、比較材料が不足する場合は`unknown`とする。

`stable`は「使用されていない」ことを保証しない。シンボリックリンク、リパースポイント、ディレクトリ、消失済みパス、snapshot取得失敗は`unknown`へ倒す。ファイル内容は読み取らない。

SQLite v6では`stable`／`changing`／`unknown`だけを許可する。v5以前の履歴は`NULL`のまま保持し、「未記録」と「観測したが不明」を区別する。

## 追加アプリ定義

カタログ`2026.08.2`でAdobe Media CacheとBlender Asset Library Cacheを追加した。

AdobeはmacOSの`~/Library/Application Support/Adobe/Common`とWindowsの`%APPDATA%/Adobe/Common`配下について、`Media Cache Files`と`Media Cache`だけを分類する。`Adobe/Common`全体や設定で変更された場所は分類しない。

BlenderはAsset Library索引キャッシュだけを分類する。macOSの`/Library/Caches/Blender`はファイルシステムルート基準、Windowsは`%LOCALAPPDATA%/Blender Foundation/Blender/Cache`を使う。物理シミュレーション、レンダー一時データ、自動保存、外部キャッシュは対象外とする。

DaVinci ResolveとAutodesk Flameは保存場所が設定や共有ストレージ構成に依存するため、設定を読み取れるまで固定パス定義を追加しない。

## 保存済み分類とUI

UI向けqueryは完了済みスキャンだけを対象とし、既定100件、最大500件でページングする。保存済みカタログ版と現在の組み込み版が一致し、定義ID・定義版も一致する場合だけ現在の定義詳細を返す。

アプリキャッシュ画面はアプリ名、分類、パス、実使用量推定、信頼度、観測状態、根拠、再生成可否、整理時影響、カタログ版・定義版を表示する。アプリ内削除・移動は提供せず、表示サイズが実際に解放できる容量を保証しないことを明記する。
