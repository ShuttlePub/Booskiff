# 画像メタデータ 状態遷移表

> **設計文書（未実装）**。稼働コード・migration・route・`openapi.json` は本 unit で変更しない。
> 用語・型は [README.md](README.md)、route は [api.md](api.md)、検証項目は [test-vectors.md](test-vectors.md)。

## 1. 状態の定義

file ごとに次の 2 つの状態機械がある。

### 1.1 許諾の固定状態 `license.state`

| 状態 | 意味 | 許諾の変更 | 備考 |
| --- | --- | --- | --- |
| `unfixed` | 未固定。メタデータ行が無い file もこの状態（revision 0・未指定） | 可（所有者のみ） | 初期状態。既存画像はすべてここから始まる |
| `reserved` | ShuttlePub が「他者向け投稿として確定する直前」に予約中 | **不可**（409 `license_reserved`） | 予約には TTL がある。タグ・分類の更新は可 |
| `fixed` | 初回の他者向け共有で固定済み | **不可**（409 `license_fixed`）。同一内容の再指定のみ no-op で 200 | 不可逆。unpublish・file 削除・連合配送失敗で戻らない |

**保存と導出**: `reserved` は file のメタデータ行には保存しない。`fixed`（`fixed_at` が非 NULL）以外で、その file に有効な予約ロック（予約項目の `lock_active`。api.md §2.2）が 1 件あれば `reserved`、無ければ `unfixed` と**導出**する。
これにより、メタデータ行が無い file（revision 0）にも予約でき、`reserve`・`release` は行を作らず revision も変えない。行が作られるのは、最初の内容変更（PATCH）・`commit`・backfill のときだけである。

### 1.2 予約 `reservation.status`

| 状態 | 意味 | 終端 |
| --- | --- | --- |
| `active` | 予約中。対象 file のすべてに予約ロックを保持している | いいえ |
| `committed` | 確定済み。対象 file はすべて `fixed` | はい |
| `released` | ShuttlePub が明示的に解放（投稿拒否など）。対象 file のロックを解く（`unfixed` に戻る） | はい |
| `lapsed` | TTL 経過後に、別の主体（所有者の許諾変更・別予約）が**ある 1 file のロック**を必要としたため、その項目のロックが失効した。1 項目でも失効した予約は `lapsed` で、`commit` はできない（全 file が対象。§4.4） | はい |

`expires_at` を過ぎても、誰もそのロックを必要としない間は DB 上 `active` のまま残る（**遅延失効**）。
この間に ShuttlePub が `commit` した場合は成功する（§4.4）。TTL は「ロックの最長保持時間」であり、
「投稿の確定可否の判定」ではない。**失効は項目（file）単位**で、他の file のロックを取らずに行える（ロック順の逆転・デッドロックを避ける）。

## 2. 許諾の固定状態の遷移表

操作主体: **O** = 所有者（Account JWT）、**SP** = ShuttlePub（サービス JWT）、**BK** = Booskiff 内部（遅延失効・file 削除）。

| # | 現在 | 操作（主体） | 条件 | 遷移後 | revision | 応答 |
| --- | --- | --- | --- | --- | --- | --- |
| T01 | `unfixed` | 許諾を別の版参照へ変更（O） | `expected_revision` 一致・選択可能な版 | `unfixed`（許諾更新） | +1 | 200 |
| T02 | `unfixed` | 同じ許諾を再指定（O） | 値が現状と同一 | `unfixed` | 変化なし | 200（no-op） |
| T03 | `unfixed` | `expected_revision` 不一致（O） | — | 変化なし | 変化なし | 409 `revision_mismatch` |
| T04 | `unfixed` | `reserve`（SP） | 所有者一致・他の有効な予約なし | `reserved`（行なしでも可。**行は作らず** revision 0 のまま） | 変化なし | 201 |
| T05 | `unfixed` | `reserve`（SP） | 別の `active`（未失効）予約が存在 | 変化なし | 変化なし | 409 `file_reserved` |
| T06 | `unfixed` | `reserve`（SP） | 所有者不一致・file 不在（所有者限定は暫定の技術制限。README §9「他 Account 再利用」は未決） | 変化なし | 変化なし | 404（区別しない） |
| T07 | `reserved` | 許諾変更（O） | 予約が未失効 | 変化なし | 変化なし | 409 `license_reserved` |
| T08 | `reserved` | 許諾変更（O） | 予約が TTL 超過（遅延失効） | `unfixed`（許諾更新）。当該項目のロックが失効し予約は `lapsed`。同じ予約の他 file のロックは残る | +1 | 200 |
| T09 | `reserved` | タグ・分類の更新（O） | `expected_revision` 一致 | `reserved`（タグ・分類更新） | +1 | 200 |
| T10 | `reserved` | `commit`（SP） | 予約が `active`（TTL 内外を問わず） | `fixed`（行なしなら未指定 v1 の行を作成） | +1（行なしは 0→1） | 200 |
| T11 | `reserved` | `release`（SP） | 予約が `active` | `unfixed`（行は作らない・変更しない） | 変化なし | 200 |
| T12 | `reserved` | `reserve`（SP・別予約） | 既存予約が TTL 超過 | 旧予約の当該項目が失効し旧予約は `lapsed`、新予約で `reserved` | 変化なし | 201 |
| T13 | `reserved` | file 削除（O） | 予約が未失効 | 変化なし | 変化なし | 409 `file_reserved` |
| T14 | `reserved` | file 削除（O） | 予約が TTL 超過 | メタデータ行（あれば）ごと削除。当該項目は失効し予約は `lapsed` | — | 204 |
| T15 | `fixed` | 許諾を**別**の版参照へ変更（O） | — | 変化なし | 変化なし | 409 `license_fixed` |
| T16 | `fixed` | 同じ許諾を再指定（O） | 値が同一 | `fixed` | 変化なし | 200（no-op） |
| T17 | `fixed` | タグ・分類の更新（O） | `expected_revision` 一致 | `fixed`（更新） | +1 | 200、フィード通知あり |
| T18 | `fixed` | `reserve`（SP） | 所有者一致 | `fixed`（ロックしない）。項目は `already_fixed: true` | 変化なし | 201 |
| T19 | `fixed` | `commit`（SP） | 予約内の項目 | `fixed` | 変化なし | 200 |
| T20 | `fixed` | unpublish / publish（O） | — | `fixed` のまま | 変化なし | 既存 API の応答 |
| T21 | `fixed` | file 削除（O） | — | メタデータ行も削除。ShuttlePub 側のスナップショットは残る | — | 204、`deleted` 通知 |
| T22 | `fixed` | 連合配送失敗（外部事象） | — | `fixed` のまま | 変化なし | （Booskiff は関知しない） |
| T23 | 任意 | backfill-fixed（SP） | 既存画像の移行手順（§6） | `fixed`（許諾は現状値のまま） | 行を作成/更新したとき +1 | 200 |

`reserved` / `fixed` の file に対する **所有者の PATCH でタグ・分類だけを変える**操作は常に可能（T09, T17）。許諾フィールドを含む PATCH は、
許諾値が現状と同一なら許諾については no-op、異なれば T07/T15 で 409 になる（同じリクエストのタグ・分類も適用されない。リクエスト全体が原子的）。

## 3. 予約の遷移表

| # | 現在 | 操作 | 遷移後 | 応答・備考 |
| --- | --- | --- | --- | --- |
| R01 | （なし） | `reserve`（新しい `idempotency_key`） | `active` | 201。対象 file を ID 昇順でロックし全件原子的に `reserved` |
| R02 | `active`/`committed`/`released`/`lapsed` | `reserve`（同じ `idempotency_key`・同一内容） | 変化なし | 200。現在の `status` と最新のスナップショットを返す（応答喪失の再送） |
| R03 | 任意 | `reserve`（同じ `idempotency_key`・**異なる**内容） | 変化なし | 409 `idempotency_key_reused` |
| R04 | `active` | `commit` | `committed` | 200。対象 file が `fixed`（`already_fixed` 項目は変化なし） |
| R05 | `committed` | `commit`（再送） | 変化なし | 200。確定結果を再返却（冪等） |
| R06 | `active`（TTL 超過・未失効） | `commit` | `committed` | 200。遅延失効なので成功（§4.4） |
| R07 | `lapsed` | `commit` | 変化なし | 409 `reservation_lapsed`。**何も確定しない**。ShuttlePub は再予約（§4.4）。投稿は `commit` 成功まで他者に見せない契約なので、公開済みの条件は変わらない |
| R08 | `released` | `commit` | 変化なし | 409 `reservation_released` |
| R09 | `active` | `release` | `released` | 200。対象 file は `unfixed` に戻る（`already_fixed` 項目は変化なし） |
| R10 | `released` | `release`（再送） | 変化なし | 200（冪等） |
| R11 | `committed` | `release` | 変化なし | 409 `reservation_committed`。固定は解除できない |
| R12 | `lapsed` | `release` | 残っている項目のロックを解く | 200（冪等）。失効していない項目（同じ予約の他 file）のロックもここで解放される |
| R13 | 任意 | 所有者が一致しない `owner` で `commit`/`release`/`reserve` | 変化なし | 404（予約の存在を秘匿） |

`commit` は予約内の**全 file**を原子的に確定する。一部の file が欠けている（削除済み）場合は、何も確定せず 409 `file_missing` とする。予約は `active` のまま残り、
残りの file のロックも残る。ShuttlePub は **まず旧予約を `release`**（欠けた file は無視して残りのロックを解く）してから、欠けた file を除外して新しい `idempotency_key` で再予約する。
`release` せずに再予約すると、旧予約のロックが未失効の間は 409 `file_reserved` になる（投稿が削除済み file を参照している異常系）。

## 4. シナリオ別の扱い

固定のトリガーは ShuttlePub が決める「他者向け投稿として確定する直前」である。Booskiff 自身の `publish`（`/public/{key}` 発行）や upload は固定のトリガーにならない（d02-q06）。

標準の流れ（ShuttlePub 視点）:

```
投稿の検証 ──→ reserve(idempotency_key, files) ──→ 投稿を ShuttlePub DB に「未公開（pending）」で保存
                    │                                  （reservation_id を同一トランザクションで保存。他者には見せない・配送しない）
              失敗・拒否 → release                                  │
                                                       保存成功 → commit(reservation_id)
                                                                  │ 成功
                                                         commit 応答を attachment スナップショット
                                                         (revision, license, tags, categories) として保存
                                                                  │
                                                         投稿を公開（他者に表示）→ 連合配送（Emumet 経由。失敗しても固定は解けない）
```

固定は「投稿が ShuttlePub に未公開で永続化された後」に行う。永続化の前に `commit` すると、永続化に失敗した場合に未共有の file が固定されてしまう（不可逆）ため採用しない（README の比較表を参照）。

**契約（ShuttlePub への要求）: 投稿は `commit` が成功するまで、他者に表示・連合配送してはならない。** これにより、閲覧者が見る許諾は常に固定済みの値だけになり、
公開済みの条件が後から変わる経路が無くなる（§4.4）。`commit` が失敗した投稿は、まだ誰にも見えていないので、再予約して最新の許諾で公開できる。

### 4.1 投稿の種類

| シナリオ | 固定 | 扱い |
| --- | --- | --- |
| 限定共有（フォロワー限定・メンション限定など、**自分以外の誰かに見える**投稿） | する | 公開範囲を問わず他者向け。`reserve`→`commit`（d02-q06） |
| 下書き | しない | 予約も取らない。許諾は自由に変更できる |
| 自分だけの投稿 | しない | 他者に見えない間は他者向けではない。後から他者向けへ変更するタイミングで `reserve`→`commit` |
| 予約投稿（スケジュール） | 送信時に行う | 登録時には予約もしない。ShuttlePub が実際に配信可能にする直前に `reserve`。登録から送信までの間は許諾を変更できる |
| 既に固定済みの file を再利用する別の投稿 | 変化なし | `reserve` が `already_fixed: true` で成功し、`commit` も no-op |
| 投稿の編集で添付を追加 | 追加分のみ | 追加された file だけを新しい `idempotency_key` で `reserve`→`commit` |

「他者向け」の判定（公開範囲の分類）は ShuttlePub の責務で、Booskiff は判定しない。

### 4.2 失敗・拒否・応答喪失

| シナリオ | 結果 |
| --- | --- |
| 投稿が拒否された（検証エラー・ポリシー拒否・ShuttlePub 内の保存失敗） | `release`。対象 file は `unfixed` に戻る。再送しても冪等（R10） |
| `reserve` の応答が失われた | 同じ `idempotency_key`・同一内容で再送 → 同じ予約が返る（R02）。二重予約・二重ロックにならない |
| `commit` の応答が失われた | `reservation_id` で `commit` を再送 → 確定結果が返る（R05）。この間 `release` を送ってはならない（R11 で 409） |
| `release` の応答が失われた | `release` を再送 → 冪等（R10） |
| ShuttlePub が `reserve` 後に消えた（クラッシュ） | TTL 経過後、所有者の許諾変更または別予約が遅延失効を起こす。ShuttlePub が復旧して投稿が（未公開で）保存済みなら `commit`（R06、失効済みなら §4.4）、未保存なら `release` または放置 |
| ShuttlePub が投稿を未公開で保存した後・`commit` 前にクラッシュ | ShuttlePub の復旧処理が保存済みの `reservation_id` で `commit` を再送する（R04/R06）。`lapsed` だった場合は §4.4。この間、投稿は未公開のまま |
| 連合配送の失敗（Emumet 経由の送信失敗・相手サーバー拒否） | 固定は解けない。許諾は固定のまま、配送の再試行は ShuttlePub/Emumet の責務。`release` してはならない（既に `committed`） |
| 同じ file を使う 2 つの投稿が同時に確定しようとする | 後から来た `reserve` が 409 `file_reserved`。ShuttlePub は先行側が `commit`/`release` されるまで待って再試行する。先行が `commit` 済みなら後続は `already_fixed` で成功する |
| 所有者が確定直前に許諾を変更しようとする | 予約中は 409 `license_reserved`。`reserve` 前に変更が成功していれば、その値が予約時のスナップショットになる |
| タグ・分類が `reserve` と `commit` の間に更新された | 更新は成功し revision が進む。`commit` 応答のスナップショット（最新）を ShuttlePub は保存する |

### 4.3 同時変更の直列化

- すべての変更系操作（PATCH・reserve・commit・release・backfill・file 削除）は、対象 file の `files` 行を `SELECT ... FOR UPDATE` してから行う。メタデータ行が無くても `files` 行は存在するので、「行が無い」file の初回変更同士も直列化される。
- 複数 file を扱う操作（reserve/commit/release/backfill）は file ID 昇順でロックを取り、デッドロックを避ける。
- 所有者の PATCH は加えて `expected_revision` による楽観排他を行う。ロック取得後に revision を比較するので、同時 PATCH のうち 1 つだけが成功し、他は 409 `revision_mismatch`。

### 4.4 失効（lapse）と遅い commit

予約の TTL（既定 3600 秒、最大 86400 秒）は、ShuttlePub が消えた場合にロックが永久に残らないための上限である。

- TTL 超過後でも、**その file のロックを必要とする操作が起きない限り**予約は `active` のまま。ShuttlePub の遅い `commit` は成功する（R06）。再予約を避けるための緩和で、固定する許諾は予約時と同じ値である（その間に所有者が許諾を変えていれば、次項の失効になる）。
- 所有者が TTL 超過後に許諾を変更した（T08）、または別の `reserve` が来た（T12）場合、その file の項目のロックが失効し、予約は `lapsed` になる。その後の `commit` は 409 `reservation_lapsed`（R07）で、**予約内のどの file も確定しない**。
- **公開済みの条件は変わらない**: ShuttlePub は `commit` が成功するまで投稿を他者に見せない（§4 の契約）ので、`lapsed` になった時点で投稿を見た人はいない。したがって、失効後の再予約で許諾が変わっていても、既に他者に示された条件を変えることにはならない。
- ShuttlePub は R07 を受けたら、(1) 旧予約を `release`（失効していない他 file のロックを解く。R12）、(2) 新しい `idempotency_key` で再 `reserve`、(3) 応答のスナップショット（所有者が変更した最新の許諾）を取り込んで `commit`、(4) `commit` 成功後に投稿を公開する。許諾が変わっていた場合は、公開前の投稿の添付スナップショットが最新の許諾になるだけである（必要なら投稿者に通知する。通知方法は ShuttlePub の契約 unit で決める）。
- 契約に反して ShuttlePub が `commit` 前に投稿を公開した場合の保護は Booskiff にはない（Booskiff は投稿の可視性を知らない）。この要求は ShuttlePub の契約テストで検証する（test-vectors S28）。

## 5. 既存画像の状態例（E1〜E6）

既存画像のデータは移行で書き換えない（d02-q08）。Booskiff の移行は**テーブルの追加のみ**で、既存 file にもメタデータ行を作らない。
メタデータ行が無い file は「未指定・未固定・revision 0・タグなし・分類なし」とみなす（README §2.2）。

| # | 既存 file の状況 | 導入直後の状態 | 後設定 | 備考 |
| --- | --- | --- | --- | --- |
| E1 | 一度も他者向け投稿で共有していない（アイコン/バナーとして使用中の file を含む） | 未指定・`unfixed`・revision 0（行なし） | 所有者が許諾・タグ・分類を設定可能 | アイコン/バナー（Emumet の media-upload 範囲）は投稿添付ではないので固定しない。同じ file を後で投稿添付に使えば通常の `reserve`→`commit` |
| E2 | 導入前に他者向け投稿で共有済み | **backfill で固定**: 未指定のまま `fixed` | 許諾は不可（タグ・分類は可） | 「既共有は固定」。backfill は ShuttlePub が共有済み file の ID を渡して実行（§6）。許諾に CC 等を後から付けられない |
| E3 | 未共有で、所有者が導入後に許諾を設定した | 設定値・`unfixed`・revision ≥ 1 | 共有されるまで何度でも変更可 | 「未共有だけ後設定可能」 |
| E4 | `publish` 済み（公開 URL あり）だが投稿には未共有 | E1 と同じ（`unfixed`） | 可 | `/public/{key}` の発行・unpublish は固定のトリガーではない（d02-q06）。メタデータ変更で bytes・URL は変わらない |
| E5 | 同一 bytes の別 file が共有済みで、こちらは未共有 | `unfixed`（行なし）。共有済み側の固定はこちらに影響しない | 可 | 同一 bytes でも別 file は別々（d02-q10）。bytes の同一性でメタデータを統合・継承しない |
| E6 | 下書き・予約投稿・自分だけの投稿にだけ添付されている | `unfixed` | 可 | 他者向けになる直前（予約投稿なら送信時）まで固定されない。backfill の対象外（ShuttlePub は他者向け投稿の添付だけを渡す） |

E2 と E3 は同じ「導入後の状態」でも固定済みかどうかで許諾の編集可否が分かれる。そのため backfill が完了する前に所有者向け許諾の**書込み**を有効にしてはならない（§6 手順 1・5）。

## 6. 既存画像の backfill 手順（実行は後続 unit。本 unit は手順の設計のみ）

目的: 導入前に他者向け投稿で共有済みの file（E2）を、未指定のまま固定状態にする。Booskiff は投稿を知らないので、共有済み file の特定は ShuttlePub が行う。

1. **Booskiff 側の schema 追加を先に適用**（migration は追加のみ。既存 `files` 行を更新しない）。この時点では所有者向け許諾の書込みは**無効**（設定 `BOOSKIFF_MEDIA_LICENSE_WRITE_ENABLED=false`、名称は後続 unit で確定）。タグ・分類の PATCH は許諾に関係しないので有効にしてよい。
2. ShuttlePub が、他者向け投稿（下書き・自分だけ・未送信の予約投稿を除く）に添付されている file ID を所有者ごとに集める。
3. ShuttlePub が `POST /v1/internal/media-metadata/backfill-fixed` を所有者ごと・最大 500 件ずつ呼ぶ。まず `dry_run: true` で結果件数を確認し、問題なければ `dry_run: false`。各 file の結果は `fixed` / `already_fixed` / `not_found` / `skipped_reserved`。冪等で、途中で止まっても同じ入力で再実行できる。
   - 既存のメタデータ行がある `unfixed` file は、**設定済みの許諾のまま**固定する（上書きしない）。
   - 行が無い file は、未指定（`unspecified` v1）の行を `fixed`・revision 1・`fixed_via = "backfill"` で作成する。
4. 全件の結果を突合する（ShuttlePub が集めた ID 数 = `fixed` + `already_fixed` + 説明可能な `not_found`）。`not_found` は削除済み file またはオーナー不一致。`skipped_reserved`（backfill と同時に `reserve` が走った file）は、予約が終わった後に再実行して 0 件になるまで繰り返す。
5. 完了を確認してから許諾の書込みを有効化する。有効化後に新しく共有される file は、通常の `reserve`→`commit` で固定される。
6. 切り戻し: backfill が書くのはメタデータ行のみで、画像 bytes・`files` 行・公開 URL は変わらない。固定は不可逆なので、誤って固定した行は運用手順（管理者による DB 修正）でのみ戻せる。この運用は初回外であり、`dry_run` を必ず先に行う理由になる。

backfill と `reserve` が同じ file に同時に来ても `files` 行ロックで直列化され、先行した方で結果が決まる:
- backfill が先: file は `fixed`。`reserve` は 201・`already_fixed: true`（ロックなし）。
- `reserve` が先: backfill はその file を **`skipped_reserved`**（固定せず、予約も壊さない）。予約が `commit` されれば固定され、`release`・失効なら未固定に戻る。ShuttlePub は `skipped_reserved` の file を後で backfill し直す（手順 4 の突合で 0 件になるまで繰り返す）。

## 7. 変更フィード（通知）との関係

| 条件 | フィードに出るか |
| --- | --- |
| `fixed` の file でタグ・分類が実際に変わった | 出る（`kinds` に `tags` / `sensitive_categories`） |
| `fixed` の file の PATCH が no-op（値が同じ） | 出ない（revision も進まない） |
| `unfixed` / `reserved` の file の変更 | 出ない。ShuttlePub がまだ参照しておらず、`commit` 応答が最新のスナップショットを返すため |
| `commit` による固定そのもの | 出ない（ShuttlePub 自身が起こした変更） |
| `fixed` の file が削除された | 出る（`kinds` に `deleted`）。ShuttlePub は最後に取得したスナップショットを保持し、許諾の表示を取り消さない |
| backfill による固定 | 出ない |

フィードは通知のみ。真実は `batch-get` の一括取得である。ShuttlePub は添付ごとの `applied_revision`（取り込み済みの revision）を保持し、
`revision > applied_revision` の添付を対象に Update を送り、成功した添付だけ `applied_revision` を進める。失敗した添付だけを再試行する（d11-q04）。
通知を取りこぼしても、定期的な `batch-get` の突合（全添付の `revision` と `applied_revision` の比較）で回復できる。

## 8. D11 反映範囲（案 B で確定）との関係

D11 の反映範囲は、ユーザー本人の回答で**案 B（同じ Account の全 Profile の投稿に反映）に確定**した。本表の遷移は Profile に依存しない。
フィードの通知は file 単位の 1 件で、ShuttlePub は `file_id` を使う同じ Account の全添付を対象にする。
不採用の案 A との比較は [README.md §9](README.md#9-d11-反映範囲は案-b-で確定)。
