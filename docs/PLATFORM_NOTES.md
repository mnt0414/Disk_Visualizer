# プラットフォーム特性と実装指針

この文書はDisk Visualizerの走査、容量算出、境界判定、差分更新、権限設計に影響するmacOS／Windowsの特性をまとめる。

## 共通原則

- ファイル名、パス、属性、ファイルシステム構造は信頼できない入力として扱う。
- ファイル内容を開かず、メタデータのみ参照する。
- シンボリックリンク、ジャンクション、リパースポイントは既定で追跡しない。
- 走査ルートと別ボリュームへ自動で降りない。
- 論理サイズと割り当て済みサイズを分離する。
- 共有ブロック、クローン、スナップショット、圧縮、スパースにより、ファイル単位の合計と実際に解放可能な容量は一致しない。
- OS別処理は`cfg(target_os = ...)`で隔離し、両OSのネイティブCIで検証する。

## macOS

### ファイルシステム

- APFSはmacOS High Sierra以降の既定ファイルシステム。
- APFSにはclones、snapshots、space sharing、sparse filesがある。
- HFS+もサポート対象として残る。
- 外付けストレージではexFATも考慮する。
- APFS volume間でcontainerの空き容量を共有できるため、単一directory treeの集計はvolume/container全体の空き容量説明と分離する。
- APFS cloneやsnapshotの共有extentは単純なファイル合計では正確に「解放可能量」へ変換できない。

### サイズ取得

- Unix metadataの`st_size`を論理サイズとして扱う。
- `st_blocks * 512`を割り当て済みサイズの推定として扱う。
- 値は「実使用量推定値」であり、APFS clone、snapshot、compressionの共有を完全には表さない。
- block数やサイズの整数変換はoverflowを検査する。

### identityとパス

- file identityはdevice/volume identityとinode/file identityの組で扱う。
- `/var/...`が`/private/var/...`へcanonicalizeされる例がある。テストで一時ディレクトリの表示パスと実体パスが異なる可能性を考慮する。
- macOSは通常case-insensitiveだが、case-sensitive APFSも存在する。アプリキャッシュのmacOSパス照合を無条件にcase-insensitiveへしない。
- Unicode normalization差を前提に、表示名とidentityを混同しない。

### linksとvolume境界

- symlinkは追跡しない。
- APFSのvolume構成やfirmlink相当の見え方により、表示パスだけで同一volumeを推測しない。
- メタデータから取得したvolume identityを基準に境界判定する。
- canonicalizeは対象が変動する場合があるため、表示用途、境界用途、identity用途を分ける。

### 権限

- 起動時にFull Disk Accessを要求しない。
- 読み取れない項目はskip reasonとして記録し、結果の欠落を0 byteに見せない。
- Full Disk AccessはユーザーがSystem Settingsで付与する。常駐root helperは導入しない。

### 差分更新（将来）

- FSEventsは大きなdirectory treeの変更監視に適する。
- event履歴は基準scanの代替ではない。履歴の欠落、event ID不連続、volume identity変化を検出したら部分／full rescanへ戻す。
- renameや複数変更の解釈をパス文字列だけに依存しない。

## Windows 11

### ファイルシステム

- 内蔵volumeの主対象はNTFS、外付けではexFATを扱う。
- NTFSにはhard link、sparse file、compression、reparse point、junction、USN Change Journalがある。
- exFATではNTFS固有機能と差分更新の可否を仮定しない。

### サイズ取得

- 論理サイズとallocated sizeを分ける。
- 現実装はfile handleを開いたあと`GetFileInformationByHandleEx(FileStandardInfo)`の`AllocationSize`を参照する。
- `FILE_STANDARD_INFO.AllocationSize`はfileに割り当てられた領域を表す。
- FFI fieldの型はwindows crateの生成型を正とする。SDK資料のC表現をそのままRustのfield accessへ移さない。
- 失敗時はlogical sizeへ静かに置換せず、取得不能を記録できる設計を維持する。

### identityとTOCTOU

- volume serial/identity + file IDをhard-link dedup keyとする。
- pathに対する複数回のmetadata lookupは、走査中の置換で別objectを参照するTOCTOUを生む。
- file handleからsize、identity、attributesを同じsnapshotとして取得する。
- handle取得前後のpath変化を完全には防げないため、検証不能時は未確認／変動中として扱う。

### paths

- Windows pathはcase-insensitive照合が必要な場面があるが、表示値のcaseは保持する。
- drive letter、UNC、Win32 namespace、long pathを考慮する。
- 古いWin32 APIの`MAX_PATH`前提を持ち込まない。wide APIとlong-path対応を確認する。
- `C:`と`C:\`、区切り文字、prefix境界を文字列prefixだけで判定しない。
- アプリキャッシュルートは`USERPROFILE`、`LOCALAPPDATA`、`APPDATA`、`WINDIR`などの環境由来値から解決し、path component境界で比較する。

### reparse pointsとvolume境界

- `FILE_ATTRIBUTE_REPARSE_POINT`を検出し、既定で追跡しない。
- symlink、junction、mounted folderなどを同じ「普通のdirectory」として再帰しない。
- reparse pointを開く場合の`FILE_FLAG_OPEN_REPARSE_POINT`有無で対象が変わる。
- volume identityをruntimeで取得してrootと比較する。test fixtureのpath表現だけでvolume境界を模擬しない。

### 権限

- メインUIは標準権限のまま動作させる。
- 将来の保護領域scanはユーザー選択時だけUACで一回限りのscanner processを起動する。
- 昇格processには走査rootと読み取り設定だけを渡し、任意commandやwriteを許可しない。
- IPCはpeer、message type、size、pathを検証する。

### 差分更新（将来）

- NTFSではUSN Change Journalを候補とする。
- Journal ID、開始USN、履歴範囲、volume identityの連続性を検証する。
- journal削除、wrap、volume変更、offline中の履歴欠落ではfull/partial rescanへ戻す。
- reparse point処理はUSN recordの解釈でも別途考慮する。

## exFAT

- macOS／Windowsの外付け媒体で共通に現れる。
- NTFS/APFS固有のidentity、compression、journal、clone機能を仮定しない。
- 差分更新を提供できない場合はmetadata rescanへフォールバックする。
- 切断、再接続、別machineでの変更を想定し、volume identityと基準scanの信頼を再確認する。

## 実装・レビュー時チェックリスト

- [ ] OS別native APIのreturn typeを実際のRust bindingで確認したか
- [ ] size/ID変換にoverflow、negative値、取得失敗の扱いがあるか
- [ ] link/reparse pointをmetadata取得前後で意図せずfollowしていないか
- [ ] root外、別volume、path prefix collisionを拒否しているか
- [ ] Windows pathのcase差とseparator差をtestしたか
- [ ] macOS canonical path差をtestしたか
- [ ] sparse、compressed、hard link、deep tree、long pathをtestしたか
- [ ] skip reasonを保存・表示できるか
- [ ] 新規DBとmigration後DBのschema/indexが一致するか
- [ ] macOSとWindowsのnative CIが成功したか

## 公式資料

### Apple

- APFS Guide FAQ: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html
- File System Programming Guide: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemDetails/FileSystemDetails.html
- File System Events: https://developer.apple.com/documentation/coreservices/file_system_events
- stat(2): https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/stat.2.html

### Microsoft

- FILE_STANDARD_INFO: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_standard_info
- GetFileInformationByHandleEx: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex
- Reparse Points and File Operations: https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-points-and-file-operations
- Naming Files, Paths, and Namespaces: https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
- Change Journals: https://learn.microsoft.com/en-us/windows/win32/fileio/change-journals
