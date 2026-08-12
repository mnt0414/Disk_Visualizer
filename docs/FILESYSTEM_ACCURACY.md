# Filesystem accuracy

Phase 4の最初の実装として、論理サイズとは別に実際の割り当て済みサイズを計測する。

- macOS/Unix: `st_blocks * 512`
- Windows: `GetCompressedFileSizeW`
- ハードリンク: ボリュームIDとファイルIDの組み合わせで重複を除外
- スパースファイル: 割り当て済みサイズが論理サイズより小さい場合に識別
- 圧縮ファイル: Windowsのファイル属性から識別
- シンボリックリンク: 追跡せず、読み飛ばしとして計上
- Windowsジャンクション／リパースポイント: `FILE_ATTRIBUTE_REPARSE_POINT`で検出し、追跡しない

進捗表示は重複排除後の論理サイズを使い、保存する個別ファイルサイズは元の論理サイズを維持する。
