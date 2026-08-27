# 変更履歴tokenと連続性判定

Phase 6では、macOS FSEventsとWindows USN Change Journalの履歴位置を、SQLite v7の`history_token`へversion付き文字列として保存する。

## token形式

- FSEvents: `fsevents:v1:<event_id>`
- USN: `usn:v1:<journal_id>:<next_usn>`

未知のversion、欠損値、数値変換不能、0の識別子、負のUSNは破損tokenとして拒否する。共通層はOS APIを呼ばず、adapterから渡された利用可能範囲との整合だけを判定する。

## 連続性

FSEventsは、保存event IDが現在利用可能な最古event ID以上かつ最新event ID以下の場合だけ連続とする。最古event IDより前なら履歴欠落、最新event IDより後または範囲逆転なら不正範囲とする。

USNは、保存journal IDと現在journal IDが一致し、保存next USNが`lowest_valid_usn`以上かつ現在`next_usn`以下の場合だけ連続とする。Journal再作成、履歴trim、範囲逆転は差分更新不可とする。

FSEvents tokenとUSN範囲の組み合わせなど、sourceが一致しない場合も差分更新不可とする。

この判定で`continuous`になっても、volume identityとroot identityの照合は別途必須である。すべての証拠が揃った場合だけ`index_trust`が差分更新を許可する。
