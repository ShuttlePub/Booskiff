# 画像メタデータ テストベクトル

> **設計文書（未実装）**。後続の実装 unit が自動テストへ落とすための期待値表。稼働コード・migration・`openapi.json` は本 unit で変更しない。
> 型・route は [api.md](api.md)、状態は [state-machine.md](state-machine.md)、全体像は [README.md](README.md)。

## 0. 前提と記法

- アカウント: **Alice**（owner `account/acc_alice`）、**Bob**（`account/acc_bob`）。`SP` = ShuttlePub のサービス JWT。`Admin` = 管理者 token。
- file: **FA1・FA2** は Alice の file（FA2 は FA1 と同一 bytes）、**FB1** は Bob の file。特記がなければ導入直後（メタデータ行なし）。
- 「行なし」= `file_media_metadata` に行が無い file（revision 0・未指定・未固定）。
- 期待欄の `409 (reason)` は HTTP 409・`code = conflict`・`reason` は api.md の提案値。
- 並行テストは、同時に開始した 2 つのリクエストの結果の**集合**で判定する（順序は不定）。
- `[ShuttlePub 側]` は ShuttlePub の契約テストで確認する項目（Booskiff は関知しない）。Booskiff 側に対応する観点だけを置いている。
- D11 反映範囲は案 B（同じ Account の全 Profile に反映）で確定済み。§11 はその前提のテスト。

## 1. 既定値・モデル（D）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| D01 | FA1 は行なし | Alice が `GET /v1/files/FA1/metadata` | 200。`revision 0`・`license {unspecified, 1, unfixed, fixed_at null}`・`tags []`・`sensitive_categories []`・`updated_at null` |
| D02 | 既存 file が複数ある DB にメタデータ migration を適用 | 適用前後で `files`・`file_objects` の行数・内容を比較 | 変化なし。`file_media_metadata` は 0 行（既存画像のデータを書き換えない） |
| D03 | FA1 は行なし | `PATCH {expected_revision: 0}`（変更なし） | 200・revision 0。**行は作られない** |
| D04 | FA1 は行なし | `PATCH {expected_revision: 0, tags: ["風景"]}` | 200・revision 1・`license unspecified/unfixed`。行が作られる |
| D05 | FA1 は行なし | `PATCH {expected_revision: 5, tags: ["x"]}` | 409 (revision_mismatch)。行は作られない |
| D06 | FA1 は行なし | `PATCH {expected_revision: 0, license: {id: "unspecified"}}` | 200・revision 0（現状と同一。未指定の明示では固定も revision 増加もしない） |
| D07 | FA1 が `fixed`（CC BY 4.0）、FA2 は FA1 と同一 bytes・行なし | Alice が `GET FA2/metadata`、続けて FA2 の許諾を `cc0-1.0` に PATCH | FA2 は未指定・未固定（FA1 を継承しない）。PATCH は 200（FA1 の固定は FA2 に影響しない）。FA1 は変化なし |

## 2. 許諾カタログ（C）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| C01 | seed 済み | `GET /v1/media-metadata/licenses` | 8 件（`cc-by-4.0`・`cc-by-sa-4.0`・`cc-by-nd-4.0`・`cc-by-nc-4.0`・`cc-by-nc-sa-4.0`・`cc-by-nc-nd-4.0`・`cc0-1.0`・`unspecified`）。いずれも版 1 |
| C02 | seed 済み | CC0 と `unspecified` の定義を比較 | 別の `id`。CC0 の `title` は 6 種の CC ライセンスと区別できる表記。`unspecified` の `summary` は「追加の利用許諾を示さない」旨で、禁止や取消しを意味しない |
| C03 | — | Admin が `POST licenses {license_id: "x-studio", title, summary}` | 201。一覧に `x-studio` 版 1（`kind: custom`）が現れる |
| C04 | `x-studio` が存在 | Admin が同じ `license_id` を再度 POST | 409 (license_exists) |
| C05 | — | Admin が `license_id: "cc-by-4.0"`（preset の ID）で POST | 409 (license_exists) |
| C06 | — | Admin が `license_id: "X_Studio"` / `"studio"`（`x-` なし）で POST | 400 (validation)。作成されない |
| C07 | `x-studio` v1 を FX が参照（`fixed`） | Admin が `versions` で v2（別の `summary`）を追加 | v2 が追加される。`GET versions/1` の本文は不変。`GET FX/metadata` は **v1 のまま**（固定済みの条件は管理者の定義変更で変わらない） |
| C08 | `x-studio` v2 が最新・FA1 は `unfixed` | Alice が `PATCH license: {id: "x-studio"}`（版省略） | 200。保存・応答は `version: 2`（選択可能な最新版に解決し、明示の版で保存） |
| C09 | `x-studio` を deprecate 済み・FX が v1 を参照 | 一覧／FA1 を `x-studio` に PATCH／`GET FX/metadata` | 一覧に `x-studio` は出ない。FA1 の PATCH は 400 (license_not_selectable)。FX の参照は有効なまま（`GET versions/1` も 200） |
| C10 | FA1 が廃止済みの `x-studio` v1（`unfixed`）を参照 | `PATCH license: {id: "x-studio", version: 1}` + タグ変更 | 200（許諾は現状と同一なので no-op。タグのみ更新） |
| C11 | — | `license: {id: "nope"}` / `{id: "cc-by-4.0", version: 9}` で PATCH | どちらも 400 (unknown_license)。変更なし |
| C12 | — | `GET licenses/cc-by-4.0/versions/1` | 200。`selectable` を含まない。`Cache-Control: private, max-age=3600`（`public`・`immutable` を含まない） |

## 3. 所有者の更新（P）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| P01 | FA1 `unfixed`・revision 1・CC BY | `PATCH {expected_revision: 1, license: {id: "cc0-1.0"}}` | 200・revision 2・`cc0-1.0` v1 |
| P02 | 同上 | 同じ許諾（`cc-by-4.0`）を再指定 | 200・revision 1 のまま（no-op） |
| P03 | FA1 `unfixed` | `unspecified` → `cc-by-4.0` → `unspecified` と PATCH | すべて 200。`unspecified` への変更は固定を伴わない。毎回 revision +1 |
| P04 | FA1 revision 2・tags `["a"]` | `tags: ["a"]` を再送／`["b"]` を送る | 前者は 200・revision 2 のまま。後者は 200・revision 3 |
| P05 | 行なし | `tags: ["  #Landscape ", "ＬＡＮＤＳＣＡＰＥ", "sky"]` | 200・`["landscape", "sky"]`（NFKC・小文字化・`#` 除去・正規化後の重複除去、順序保持） |
| P06 | 行なし | `tags: ["#"]` / `["  "]` | 400 (invalid_tag) |
| P07 | 行なし | 正規化後 64 文字のタグ／65 文字のタグ／改行・`,` を含むタグ | 64 文字は 200。65 文字・改行・`,` は 400 (invalid_tag) |
| P08 | 行なし | 31 個のタグ（正規化後に重複なし） | 400 (too_many_tags)。30 個は 200 |
| P09 | — | `sensitive_categories: ["no-such"]` | 400 (unknown_sensitive_category) |
| P10 | 分類 `spoiler` が退役済み・FA1 に `spoiler` が付いている | `spoiler` を残したままタグだけ更新／別の file に `spoiler` を新規追加 | 前者は 200（分類は維持）。後者は 400 (category_retired) |
| P11 | FA1 の分類 `["insects","sexual"]` | 同じ集合を逆順で送る | 200・revision 変化なし（集合として昇順・重複なしに正規化して比較） |
| P12 | — | `expected_revision` の省略／負数／文字列 | 400 (validation)。変更なし |
| P13 | FA1 revision 3 | `expected_revision: 2` で PATCH | 409 (revision_mismatch)。変更なし |
| P14 | FA1 | 有効なタグ + 未知の分類を同一 PATCH で送る | 400。タグも適用されない（原子的） |
| P15 | FA1 `fixed`（CC BY 4.0）・revision 4 | `PATCH {expected_revision: 4, tags: ["new"], sensitive_categories: ["insects"]}` | 200・revision 5。許諾は `fixed`（CC BY 4.0）のまま |
| P16 | FA1 `fixed`（CC BY 4.0） | `license: {id: "cc0-1.0"}` + `tags: ["new"]` | 409 (license_fixed)。**タグも適用されない**。revision 変化なし |
| P17 | FA1 `fixed`（CC BY 4.0） | `license: {id: "cc-by-4.0", version: 1}`（同一）+ タグ変更 | 200（許諾は no-op・タグは更新） |
| P18 | FA1 `reserved`（未失効） | タグ更新／許諾を別の値に変更 | 前者は 200（revision +1）。後者は 409 (license_reserved) |
| P19 | 許諾の書込みが無効な運用段階 | 許諾を変更する PATCH／タグだけの PATCH | 前者は 403 (license_write_disabled)。後者は 200 |
| P20 | — | 未知のフィールドを含む本文 | 400 (invalid_request) |

## 4. 認可・存在秘匿（Z）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| Z01 | FA1 は Alice の file | Bob が `GET FA1/metadata` | 404。存在しない UUID を引いたときと**同じ形式**の応答（`code: not_found`） |
| Z02 | 同上 | Bob が `PATCH FA1/metadata` | 404。Alice の FA1 のメタデータは変化なし（revision・tags・license） |
| Z03 | — | token なし／壊れた token／未信頼 issuer の token で `GET FA1/metadata` | すべて 401 |
| Z04 | — | Alice の Account JWT で `POST /v1/internal/media-metadata/reservations` | 403 (missing_scope)。何も変わらない |
| Z05 | — | SP のサービス JWT で `PATCH /v1/files/FA1/metadata` | 拒否（owner claim が無く 401）。FA1 は変化なし |
| Z06 | — | scope の無いサービス JWT で内部 API を呼ぶ | 403 (missing_scope) |
| Z07 | FA1 `unfixed` | SP が `owner=Bob` で FA1 を `reserve` | 404。FA1 は `unfixed` のまま（ロックされない） |
| Z08 | FA1 `unfixed`・FB1 は Bob の file | SP が `owner=Alice`、`file_ids=[FA1, FB1]` で `reserve` | 全体が 404。**FA1 もロックされない**（部分適用なし）。応答は存在しない file を含む場合と区別できない |
| Z09 | Alice の reservation RS が `active` | SP が `owner=Bob` で RS を `commit`／`release` | どちらも 404。RS・FA1 は変化なし |
| Z10 | — | SP が存在しない `reservation_id` を `commit` | 404（Z09 と同じ形式の応答） |
| Z11 | FA1 `unfixed`、FA3 `reserved`、FA4 `fixed`、FA5 は存在しない ID | SP が `batch-get [FA1, FA3, FA4, FA5]` | `items` は FA4 のみ。FA1・FA3・FA5 は区別なく `missing`（未共有のメタデータを出さない） |
| Z12 | FB1 `fixed` | SP が `batch-get [FB1]` | `items` に FB1（`owner` に Bob を含む）。サービスは所有者を問わず、共有済み file を取得できる |
| Z13 | — | Admin token で `GET FA1/metadata`／Alice の JWT で `POST /v1/admin/media-metadata/licenses` | どちらも 401（token の種別が違う） |
| Z14 | Bob の private なタグ `secret-x`（未共有 file のみ） | Alice が `tag-suggestions?prefix=secret` | `own`・`shared` のどちらにも出ない |
| Z15 | FA1 `fixed`・タグあり・公開済み | 無認証で `GET /public/{key}` | 画像 bytes のみ。メタデータ（タグ・許諾・分類）を応答ヘッダにも本文にも含まない |
| Z16 | — | path の `{id}` が UUID でない | 400（既存 `files` route と同じ扱い） |

## 5. 予約・確定・解放（S）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| S01 | FA1 `unfixed`・revision 1 | SP が `reserve`（owner=Alice, ttl 省略） | 201。`status active`・`expires_at ≈ now+3600s`・FA1 は `license.state reserved`・`already_fixed false`・revision は **1 のまま** |
| S02 | S01 の続き | SP が `commit` | 200。`status committed`・FA1 は `fixed`（`fixed_at` あり）・revision 2 |
| S03 | FA1 `fixed` | SP が `reserve` | 201・`already_fixed: true`・ロックされない。続く `commit` も 200 で revision 変化なし |
| S04 | S01 の続き | SP が `release` | 200。FA1 は `unfixed`・revision 1。Alice が許諾を変更できる |
| S05 | S01 の続き | 同じ `idempotency_key`・同一内容で `reserve` を再送 | 200。**同じ `reservation_id`**。二重ロック・新しい予約なし |
| S06 | S01 の続き | 同じ `idempotency_key` で `file_ids` を変えて送る | 409 (idempotency_key_reused) |
| S07 | S02 の続き | `commit` を再送 | 200・同じ結果。revision は 2 のまま（二重加算なし） |
| S08 | S04 の続き | `release` を再送 | 200（冪等） |
| S09 | S04 の続き | `commit` | 409 (reservation_released)。FA1 は `unfixed` のまま |
| S10 | S02 の続き | `release` | 409 (reservation_committed)。FA1 は `fixed` のまま（固定は解除できない） |
| S11 | FA1 が別の予約で `reserved`（未失効）。FA2 は `unfixed` | 別の key で `reserve [FA2, FA1]` | 409 (file_reserved)。**FA2 もロックされない**（全件原子的） |
| S12 | — | `ttl_seconds` が 0／86401／省略 | 前二者は 400 (invalid_ttl)。省略は 3600 |
| S13 | — | `file_ids` が空／21 件／重複あり | すべて 400 |
| S14 | FA1 は `unfixed`、削除済みの FA9 | `reserve [FA1, FA9]` | 404（全体）。FA1 はロックされない |
| S15 | FA1 `reserved`・TTL 超過・他の操作なし | SP が `commit` | 200（遅延失効なので成功）。FA1 は `fixed` |
| S16 | FA1 `reserved`・TTL 超過 | Alice が許諾を変更 | 200。予約は `lapsed`。続く SP の `commit` は 409 (reservation_lapsed) |
| S17 | S16 の続き | SP が新しい key で再 `reserve` → `commit` | 201 → 200。応答のスナップショットは変更後の許諾。FA1 は `fixed` |
| S18 | FA1 `reserved`・TTL 超過 | 別の key で SP が `reserve` | 201。旧予約は `lapsed`、新予約で `reserved` |
| S19 | FA1 `reserved`（未失効） | Alice が許諾を別の値に変更 | 409 (license_reserved) |
| S20 | FA1 `reserved`・revision 1 | Alice がタグを更新（revision 2）→ SP が `commit` | 更新は 200。`commit` 応答のタグは更新後・revision 3 |
| S21 | FA1 `reserved`（未失効） | Alice が `DELETE /v1/files/FA1` | 409 (file_reserved)。file は残る |
| S22 | FA1 `fixed` | Alice が `DELETE /v1/files/FA1` | 204。`GET FA1/metadata` は 404。SP の `batch-get` は `missing`。フィードに `deleted` が出る |
| S23 | FA1 `fixed` | Alice が `POST /v1/files/FA1/publish` → `DELETE .../publish` | 既存 API の応答どおり。`license` は `fixed` のまま・revision 変化なし |
| S24 | FA1 `unfixed` | Alice が `POST /v1/files/FA1/publish`（Booskiff の公開 URL 発行） | 許諾は `unfixed` のまま。publish は固定のトリガーではない（d02-q06） |
| S25 | FA1 が既に `fixed` | 別の投稿用に SP が別の key で `reserve` を 2 回（順次） | どちらも 201・`already_fixed: true` |
| S26 | 限定共有・フォロワー限定の投稿 | `[ShuttlePub 側]` 公開範囲によらず他者向けとして `reserve`→`commit` を呼ぶ | 公開範囲に関係なく FA1 が `fixed` になる（d02-q06） |
| S27 | 下書き・自分だけ・予約投稿の登録時 | `[ShuttlePub 側]` `reserve` を呼ばない | FA1 は `unfixed` のまま。予約投稿は配信可能にする直前に初めて `reserve` |

## 6. 同時変更（X）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| X01 | FA1 revision 1 | Alice が `expected_revision: 1` の PATCH を 2 件同時に送る（別のタグ） | 一方が 200（revision 2）、他方が 409 (revision_mismatch)。更新の取りこぼしなし |
| X02 | FA1 `unfixed` | SP が別の key で `reserve` を 2 件同時 | 一方が 201、他方が 409 (file_reserved) |
| X03 | FA1 `unfixed` | `reserve` と Alice の許諾 PATCH を同時 | (a) reserve 先行なら PATCH は 409 (license_reserved)、(b) PATCH 先行なら reserve のスナップショットに新しい許諾が入る。**古い許諾で予約されることはない** |
| X04 | FA1 `reserved` | `commit` と Alice のタグ PATCH を同時 | 両方成功。最終 revision は +2。どちらの変更も失われない |
| X05 | FA1 `reserved` | 同じ予約への `commit` と `release` を同時 | どちらか一方だけ成功し、他方は 409。FA1 は `fixed` か `unfixed` のどちらかで一貫（中間状態なし） |
| X06 | — | 同じ `idempotency_key` の `reserve` を 2 件同時 | 予約は 1 件だけ作られ、両方のレスポンスが同じ `reservation_id` を返す |
| X07 | FA1・FA2 `unfixed` | `reserve [FA1,FA2]` と `reserve [FA2,FA1]` を同時 | デッドロックしない。一方が 201、他方が 409 (file_reserved) |
| X08 | FA1 行なし | `backfill-fixed [FA1]` と `reserve [FA1]` を同時 | 一方が固定、他方は `already_fixed`。FA1 は 1 回だけ `fixed`・行は 1 件 |
| X09 | FA1 行なし | `expected_revision: 0` の PATCH を 2 件同時 | 一方が 200（revision 1）、他方が 409。行は 1 件だけ |
| X10 | FA1 `fixed` | PATCH（タグ）と `DELETE FA1` を同時 | どちらの順でも矛盾しない（PATCH 先行なら通知後に削除、削除先行なら PATCH は 404）。孤立した行・フィード項目なし |

## 7. 失敗・応答喪失（F）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| F01 | `reserve` の応答が SP に届かない | SP が同じ key で再送 | 同じ予約が返る（S05）。二重ロックなし |
| F02 | `commit` が成功したが応答が届かない | SP が `commit` を再送（`release` は送らない） | 200・確定済み（S07）。revision 二重加算なし |
| F03 | 投稿が拒否された | SP が `release` | FA1 は `unfixed`・許諾を変更可能。再送しても 200 |
| F04 | SP が `reserve` 後にクラッシュ・TTL 超過 | Alice が許諾を変更し、その後 SP が復旧して `commit` | Alice の変更は 200。SP の `commit` は 409 (reservation_lapsed)。SP は再予約で最新の許諾を取り込む |
| F05 | FA1 `fixed`・連合配送が失敗 | `[ShuttlePub 側]` 配送を再試行／Booskiff に固定解除を要求 | 固定は解けない。Booskiff に `fixed`→`unfixed` の経路は無い（`release` は 409） |
| F06 | `commit` 中に DB エラー（トランザクション中断） | SP が再送 | 中断時は全 file が未確定（部分確定なし）。再送で全件確定 |
| F07 | フィードの `after` が保持期間より古い | `GET changes?after=old` | 409 (cursor_expired)。SP は `batch-get` で全添付を突合して回復 |
| F08 | フィードの同じ項目を 2 回受信 | `[ShuttlePub 側]` `revision <= applied_revision` の添付は無視 | 二重の Update 送信なし（冪等な consumer） |
| F09 | レート制限超過 | SP が内部 API を連続呼び出し | 429。状態は変わらず、再試行（冪等キー同一）で正常に処理される |
| F10 | 削除済み file を含む予約（TTL 超過後に削除） | `commit` | 409 (file_missing)。何も確定されない。SP は欠けた file を除いて再予約 |

## 8. 一括取得・変更フィード（B）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| B01 | — | `batch-get` に 0 件／101 件 | 400 (batch_too_large を含む validation) |
| B02 | FA1 `fixed`（revision 4） | `batch-get [FA1]` | `items[0]` の `owner`・`metadata.revision 4`・`license.state fixed` を含む |
| B03 | FA1 `fixed` | Alice がタグを変更 | フィードに `{file_id: FA1, revision, kinds: ["tags"]}` が出る。`revision` は `batch-get` の値と一致 |
| B04 | FA1 `fixed` | no-op の PATCH | フィードに出ない。revision も不変 |
| B05 | FA1 `unfixed`／`reserved` | タグ・分類を変更 | フィードに出ない |
| B06 | — | `commit`／`backfill-fixed` による固定 | フィードに出ない |
| B07 | FA1 `fixed` | `DELETE FA1` | `kinds: ["deleted"]` が出る。`batch-get` は `missing` |
| B08 | FA1 `fixed` | 短時間にタグを 3 回変更 | フィードに 1 件以上。最新項目の `revision` は最終値（`batch-get` と一致）。まとめられてもよい |
| B09 | フィードに 5 件 | `limit=2` で `next_cursor` を辿る | 5 件すべてを取りこぼしなく取得（重複は at-least-once として許容）。最後は `has_more: false` |
| B10 | 先行トランザクション（低い `seq`）がコミット前、後続（高い `seq`）が先にコミット | フィードを読む → 先行がコミット → もう一度読む | 1 回目に後続を返しても先行を飛ばさない設計（xmin 未満のみ返す）。先行は後続の読み取りで必ず現れる |
| B11 | — | メタデータ系 route（取得・一括取得・フィード）の応答ヘッダ | `Cache-Control: no-store`。`immutable` を含まない |
| B12 | FA1 のタグ更新後、SP が 3 件の添付（post 1〜3）に Update を送り post 2 だけ失敗 | `[ShuttlePub 側]` 再試行 | 成功した post 1・3 の `applied_revision` は更新済みで再送しない。post 2 だけが再試行される（d11-q04） |

## 9. 既存画像・backfill（E）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| E01 | [E1] 導入前から存在し一度も共有していない FA1 | 導入後に `GET`／許諾・タグ・分類を PATCH | 既定値（D01）。すべて設定可能 |
| E02 | [E2] 導入前に他者向け投稿で共有済みの FA1（行なし） | backfill → 許諾を `cc-by-4.0` に PATCH／タグを PATCH | backfill は `fixed`（未指定・revision 1・`fixed_via backfill`）。許諾 PATCH は 409 (license_fixed)。タグは 200 |
| E03 | E02 の続き | 同じ入力で backfill を再実行 | 結果 `already_fixed`。revision 不変（冪等） |
| E04 | FA1 行なし | `dry_run: true` で backfill | 結果の見込みは `fixed`。DB は変化なし（行なしのまま） |
| E05 | [E3] FA1 が導入後に Alice によって `cc0-1.0`（`unfixed`）に設定済み | backfill | `fixed`・許諾は `cc0-1.0` のまま（上書きしない） |
| E06 | [E3] 未共有の FA1 | 許諾を複数回変更 | すべて 200（共有されるまで可） |
| E07 | [E4] FA1 が publish 済み（`public_key` あり）・未共有 | 許諾・タグを PATCH | 200・`unfixed`。`public_key`・`is_public`・`/public/{key}` の応答は変わらない |
| E08 | [E5] FA1 が `fixed`・FA2 は同一 bytes の別 file | FA2 の許諾・タグを PATCH | 200。FA1 に影響しない。FA2 は `unfixed` のまま |
| E09 | [E6] 下書き・予約投稿・自分だけの投稿にのみ添付された FA1 | `[ShuttlePub 側]` backfill 対象に含めない | FA1 は `unfixed` のまま |
| E10 | — | backfill に他人の file／削除済み file／501 件／予約中の file | 前二者は `not_found`（区別しない）、501 件は 400、予約中は `skipped_reserved` |
| E11 | backfill 未完了 | 許諾の書込みを有効化しようとする運用手順 | 手順 4（突合）完了前は有効化しない。無効の間は P19（許諾 PATCH は 403）が成り立つ |

## 10. 既存挙動の非変更・キャッシュ（N）

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| N01 | FA1 が publish 済み | メタデータを PATCH（許諾・タグ・分類） | `files` 行の `name`・`mime_type`・`size_bytes`・`is_public`・`public_key` は不変。`/public/{key}` の本文・`Content-Length`・`Cache-Control: public, max-age=31536000, immutable` も不変 |
| N02 | 本設計 unit の PR | CI の openapi-drift | `openapi.json` に差分なし（稼働コード・migration を含まない） |
| N03 | FA1 `fixed`（CC BY 4.0）・unpublish 済み・または file 削除済み | `[ShuttlePub 側]` 添付の許諾表示 | 保持しているスナップショットの許諾（CC BY 4.0）を表示し続ける。unpublish・削除を許諾の取消しとして扱わない |
| N04 | — | `GET /v1/files/{id}`・`GET /v1/files` の応答 | 既存の `FileResponse` と同じフィールド（メタデータを追加しない） |

## 11. D11 反映範囲（G）

案 B（同じ Account の全 Profile の投稿に反映）で確定。

| ID | 前提 | 操作 | 期待 |
| --- | --- | --- | --- |
| G01 | FA1 を Alice の Profile P1・P2 の投稿が使っている | Alice がタグを更新 | フィードは FA1 について 1 件。`[ShuttlePub 側]` `file_id` を使う同じ Account の全添付（P1・P2）が `applied_revision` 更新の対象 |
| G02 | 同上 | フィード項目・`batch-get` の応答 | Profile や投稿を示す項目を含まない（file 単位）。`file_id`・`owner`・`revision`・`kinds` のみ |
| G03 | 同上 | Alice が `change_context` を含む PATCH を送る | 400 (invalid_request)（未知のフィールド。Profile 単位の絞り込みは契約にない） |
| G04 | FA1 を P1 の投稿が既に共有済み（`fixed`） | `[ShuttlePub 側]` P2 の別の投稿で FA1 を使う | `reserve` は 201・`already_fixed: true`。P2 の添付も同じ file のメタデータを参照し、以後の更新対象になる |
| G05 | FA1 は Alice の file。Bob の投稿が FA1 を使おうとする | `[ShuttlePub 側]` Bob の投稿確定時に FA1 を `reserve` | Booskiff は 404（Z07）。**他 Account による同一 file の再利用は契約上発生しない** |

## 12. 件数と網羅

| 区分 | 件数 | 主な観点 |
| --- | --- | --- |
| D 既定値 | 7 | 行なし既定・移行で書き換えない・同一 bytes 別 file |
| C カタログ | 12 | 8 選択肢・管理者独自・版追加で固定済み不変・廃止・版省略 |
| P 更新 | 20 | 版参照・no-op・正規化・固定後のタグ/分類・原子性 |
| Z 認可 | 16 | 別 file・他人 file・他人の予約・存在秘匿・サービス/所有者/管理者の分離 |
| S 予約 | 27 | 2 相・冪等・失効・削除・publish は固定しない |
| X 同時変更 | 10 | 行ロック・楽観排他・デッドロック・初回行作成 |
| F 失敗 | 10 | 応答喪失・投稿拒否・クラッシュ・連合配送失敗・フィード期限切れ |
| B 一括・フィード | 12 | 共有済みのみ・通知のみ・取りこぼし防止・失敗分再試行 |
| E 既存画像 | 11 | E1〜E6・backfill・冪等・dry-run |
| N 非変更 | 4 | bytes/URL/キャッシュ・openapi・FileResponse |
| G D11 | 5 | 案 B（全 Profile に反映）・Profile 非依存・他 Account 再利用なし |
| **合計** | **134** | |
