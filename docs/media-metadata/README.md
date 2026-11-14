# 画像メタデータ（利用許諾・通常タグ・センシティブ分類）producer 契約設計

> **設計文書（未実装）**。D02（利用許諾）・D10（センシティブ分類）・D11（画像タグ）の画像メタデータを Booskiff に置くための **producer 契約** の設計。
> 稼働コード・migration・route・`openapi.json`・UI は本 unit に含まない。本文書内の型・SQL・JSON は案で、`core/src` には存在しない。
> 各文書の読み方: 本書（モデル・責務・技術判断・未決・分割案）→ [api.md](api.md)（型・route・エラー）→ [state-machine.md](state-machine.md)（状態遷移・既存画像・backfill）→ [test-vectors.md](test-vectors.md)（検証項目）。

## 1. 目的と範囲

- 画像 file ごとに、利用許諾（版参照と固定状態）・通常タグ・共通センシティブ分類 ID を、Account 所有のメタデータとして Booskiff が保管する（d02-q07）。
- 所有者向け API（許諾・タグ・分類の更新、補完候補）と、ShuttlePub 向けの内部 API（初回共有の固定、メタデータ取得、変更通知）を定義する。
- 範囲外: DB migration・稼働 route・OpenAPI 生成物・既存画像データの移行の実行・ShuttlePub 側の投稿処理と連合配送・UI・公開後の許諾変更と履歴・管理者による分類訂正・共有（D9）・組織 Drive の権限。
- 決定済みの利用者向け方針（grill 記録 d02-q01〜q11・d10・d11）と、本設計で置く**技術判断（T1〜T11）**、**他 unit との合意事項**を、確定済みの利用者向け方針と混ぜずに区別して書く（§7〜§9）。

## 2. メタデータモデル

### 2.1 原則

| 原則 | 内容 | 根拠 |
| --- | --- | --- |
| file 単位 | メタデータは file ID に 1 対 1。投稿ごとの別条件は持たない。同一 bytes の別 file は別々に扱い、統合・継承しない | d02-q01, d02-q10 |
| Account 所有 | 所有者は `files.owner_type` / `owner_id`（Profile 概念なし）。更新できるのは所有者のみ。所有者が権利者本人であることは保証しない | d02-q09, 既存の `Owner` |
| revision 付き | メタデータ全体に file 単位の整数 revision。実際に値が変わったときだけ +1 | T5 |
| 版参照 | 許諾は `license_id` + `version`。カタログ行は不変で、管理者の定義変更は新版の追加。固定済み file の条件は変わらない | d02-q11 |
| 固定境界 | 許諾は ShuttlePub が他者向け投稿として確定する直前に、予約→確定（または解放）の 2 相で固定する。Booskiff の publish・upload では固定しない | d02-q06 |
| 固定後も更新可 | タグ・センシティブ分類は固定後も更新できる。更新は `expected_revision` で排他する | d02-q05, d11-q01 |

### 2.2 行がない file の扱い（既存画像）

`file_media_metadata` に行が無い file は、**未指定（`unspecified` v1）・未固定・revision 0・タグなし・分類なし**とみなす。API はこの既定値を返す。
Booskiff の導入 migration は**テーブル追加のみ**で、既存 file に行を作らず、既存データを書き換えない（d02-q08）。初回の変更で行が作られ revision 1 になる。
no-op の PATCH は行を作らない。状態例 E1〜E6 と backfill 手順は [state-machine.md §5・§6](state-machine.md#5-既存画像の状態例e1e6)。

### 2.3 revision

- 行なし = 0。許諾の参照・固定、タグ、分類のいずれかが**実際に変わった**ときに +1（`commit` による固定を含む。`reserve`/`release`・no-op は含まない）。
- PATCH は `expected_revision` を必須とし、不一致は 409 `revision_mismatch`。値が変わらない PATCH は revision を進めない（フィードにも出ない）。
- ShuttlePub は添付ごとに取り込み済みの revision を `applied_revision` として持つ。

### 2.4 許諾カタログと版参照

- 選択肢は **CC 4.0 の 6 種・CC0 1.0・未指定の 8 つ**＋管理者が追加する独自許諾（d02-q04, q11）。定義は [api.md §2.3](api.md#23-許諾カタログ8-選択肢管理者独自)。
- カタログ行 `(license_id, version)` は不変。管理者の定義変更は新版の追加で、廃止は選択可否の変更のみ。固定済み・未固定を問わず、既存の参照は有効なまま。
- file は `(license_id, version)` を保持する。PATCH で版を省略すると、その時点の選択可能な最新版に解決し、明示の版で保存する。
- 未指定は追加の利用許諾を示さない意味で、再利用の全面禁止でも既存許諾の取消しでもない。CC0 は未指定とは別物（権利放棄ツール）。
- 初期値は未指定。CC 等は利用者が選んだときだけ付く。独自文章・URL を利用者が入力する機能は初回外（d02-q02）。

## 3. 固定境界の設計（予約→確定／解放の 2 相）

### 3.1 流れ

1. ShuttlePub が投稿を検証し、他者向けに確定する直前に `reserve`（file を `reserved` にロック。予約中は許諾変更不可）。
2. 投稿を ShuttlePub に永続化（`reservation_id` を同一トランザクションで保存）。
3. 成功なら `commit`（`fixed`）。拒否・失敗なら `release`（`unfixed` に戻る）。
4. 連合配送（Emumet 経由）の失敗は固定を解かない。

詳細は [state-machine.md §2〜§4](state-machine.md)。

### 3.2 案の比較と推奨

| 案 | 内容 | 利点 | 欠点 | 判定 |
| --- | --- | --- | --- | --- |
| 1 相（確定のみ） | 永続化の**前**に `commit` | API が 1 つ | 永続化に失敗しても file が固定される（不可逆。未共有なのに許諾を選び直せない）。応答喪失で固定したか不明 | 不採用 |
| 1 相（永続化の後に確定のみ） | 永続化後に `commit` だけ | API が 1 つ。過剰固定がない | 永続化〜確定の間に所有者が許諾を変えると、閲覧者が見た条件と固定値がずれる。同時投稿の競合を防げない | 不採用 |
| **2 相（予約→確定／解放）** | `reserve` でロック→永続化→`commit`/`release` | 閲覧者が見た条件で固定される。拒否は解放で戻せる。応答喪失は冪等キーで再送。同時変更は行ロックで直列化 | API が 3 つ。TTL・失効の仕様が要る | **採用（推奨）** |
| Booskiff の publish で固定 | `/public/{key}` 発行で固定 | 実装が単純 | 公開 URL 発行は他者向け共有と無関係。d02-q06 に反する | 不採用 |

TTL の失効は遅延失効（誰もロックを必要としない間は遅い `commit` も成功）。理由は [state-machine.md §4.4](state-machine.md#44-失効lapseと遅い-commit)。

## 4. 責務分担

| 項目 | Booskiff | ShuttlePub | Emumet | frontends |
| --- | --- | --- | --- | --- |
| メタデータの保存・revision・所有者認可 | **正本** | 参照のみ（添付スナップショットを保持） | — | — |
| 許諾カタログ・版・管理者定義 | **正本**（管理 API） | 版参照で表示 | — | 選択 UI・固定表示 |
| 固定のトリガー（他者向け投稿としての確定） | 予約・確定・解放を実行 | **判定して呼ぶ**（公開範囲・下書き・予約投稿の区別） | — | — |
| 固定の整合（投稿拒否・応答喪失・同時変更） | ロック・冪等・原子性を提供 | `reservation_id` の保存・再送・解放 | — | — |
| 変更通知 | フィード（通知のみ）と一括取得 | フィードの購読・突合 | — | — |
| 対象投稿の特定・失敗分の再試行 | 関知しない | **添付ごとの `applied_revision`** で特定し、失敗分だけ再試行 | — | — |
| 連合表現（タグの AP 拡張・センシティブ・許諾） | 値を提供するのみ | 添付への写像・Update 生成 | 連合配送（失敗は固定に影響しない） | — |
| センシティブ分類の語彙 | **正本**（T2。ShuttlePub と要合意） | 本文・CW・外部の二値指定との統合 | — | 表示設定・折り畳み |
| 閲覧者の表示設定（隠す／通常表示） | — | 保存・判定 | — | UI |
| タグの補完 | 候補を返す（T3） | — | — | 入力 UI |
| 認可（所有者のみ変更） | Account JWT で判定 | サービス JWT で呼ぶ（T1） | — | — |

親の状態（queue・runs・packet）は child に移管しない。本設計は host の packet・intent に従う child 側の設計成果物である。

## 5. 変更通知と整合

- **通知のみ・真実は一括取得**: フィードは `file_id`・`revision`・`kinds` を知らせる at-least-once の通知。ShuttlePub は一括取得（共有済み file のみ）で現在値を得る。通知を取りこぼしても突合で回復できる。
- ShuttlePub は添付ごとの `applied_revision` で「どの添付がどの revision まで反映済みか」を持つ。`revision > applied_revision` の添付に Update を送り、成功した分だけ進める。失敗した分だけ自動再試行する（d11-q04）。
- フィードに出るのは固定済み file のタグ・分類の変更と削除のみ。未固定・予約中の変更は、`commit` 応答が最新のスナップショットを返すので通知しない。
- file 削除・unpublish・連合配送失敗は許諾の取消しではない。ShuttlePub は最後に取得したスナップショットを許諾表示の根拠に保持する。

## 6. キャッシュ方針

- `/public/{key}` の `Cache-Control: public, max-age=31536000, immutable`（`core/src/public.rs`、[drive/caching.md](../drive/caching.md)）は**画像 bytes 専用**であり、可変メタデータに流用しない。
- メタデータの取得・一括取得・フィードは `Cache-Control: no-store`。クライアントの再検証には `revision` を使う。
- 許諾カタログの版定義は本文が不変なので `private, max-age=3600` を許す（`public`・`immutable` は付けない。認証付きで、廃止状態は別の API に出す）。
- メタデータの変更は `files` 行を更新しないので、画像 bytes・`public_key`・公開 URL・`/public/{key}` の応答は変わらない。
- 公開 URL の発行・unpublish・file 削除は許諾の固定・取消しと無関係（固定のトリガーでも解除でもない）。

## 7. 技術判断（T1〜T11、推奨付き）

ユーザー向け方針ではなく、設計上の選択。推奨は設計者の案で未確定。

| # | 論点 | 選択肢 | 推奨 | 理由 |
| --- | --- | --- | --- | --- |
| T1 | ShuttlePub→Booskiff の認証 | (a) サービス JWT（信頼 issuer・`scope: media-metadata:service`・owner claim なし）(b) 共有の静的 admin token (c) mTLS | **(a)** | 既存の JWT/JWKS 検証を再利用でき、短命・失効が容易。owner claim が無いので所有者向け route を流用できず、逆に Account JWT では内部 API を呼べない。(b) は漏洩時の影響が大きい |
| T2 | センシティブ分類語彙の正本 | (a) Booskiff (b) ShuttlePub | **(a) Booskiff**。**ShuttlePub の D10 契約と要合意** | file に付く ID を保存する側が語彙の整合（未知 ID の拒否・退役）を保てる。ShuttlePub は ID を解決して表示・連合へ写像。D10 の閲覧設定・外部指定の写像は ShuttlePub 側 |
| T3 | タグ補完候補 | (a) 自分のタグのみ (b) 自分優先＋サーバー全体、**3 人以上の異なる所有者**が使う共有済みタグに限る (c) 全タグ | **(b)** | d11-q02（自分優先→サーバー全体）を満たしつつ、未共有 file や個人的なタグの漏洩を防ぐ。k=3 は目安で、運用で見直す |
| T4 | メタデータの保存形 | (a) 別テーブル `file_media_metadata`（行なし＝既定）(b) `files` に列追加 | **(a)** | 既存行を書き換えず、既存画像の移行が不要。`FileResponse`・`openapi.json` を変えない。行ロックは `files` 行で取る |
| T5 | revision | (a) file 単位の整数。変化時のみ +1、行なし=0 (b) 更新時刻 (c) 内容ハッシュ | **(a)** | `expected_revision` と `applied_revision` の比較に使え、no-op を通知しない。時刻は同時更新・時計ずれに弱い |
| T6 | 固定の相 | §3.2 の比較 | **2 相（予約→確定／解放）** | 過剰固定・条件のずれ・応答喪失・同時投稿のすべてを説明できる |
| T7 | 予約の TTL と失効 | (a) 既定 3600 秒・最大 86400 秒・遅延失効 (b) 即時失効（スイーパーで失効） | **(a)** | 永続化済みの投稿の遅い `commit` を失敗させない。ロックが必要になった時点（所有者の変更・別予約）でだけ失効させる |
| T8 | 変更フィードの方式 | (a) outbox テーブル＋カーソル取得（xmin 未満のみ返す）(b) push（webhook）(c) 論理レプリケーション | **(a)**（push は後で wake-up として追加可） | 通知のみで真実は一括取得という性質に合う。取りこぼし・順序逆転を xmin で防ぐ。at-least-once で冪等な consumer が前提 |
| T9 | タグの正規化・上限 | NFKC・trim・空白圧縮・先頭 `#` 除去・小文字化。64 文字・30 個 | **この案** | 補完の一致率を上げ、辞書の汚染を抑える。正規化規則・補完辞書の共有範囲は d11 で未決だったため、ここで案として置く（ユーザー確認が必要なら別途） |
| T10 | 競合の表現と直列化 | `files` 行の `FOR UPDATE`＋`expected_revision`、エラーに追加フィールド `reason`（既存 `code` は維持） | **この案** | 行なし file の初回変更も直列化できる。`reason` は既存クライアントの互換を壊さない追加（`ErrorDetail` の変更は後続 unit） |
| T11 | カタログの保存と管理 | (a) DB の不変テーブル＋管理 API、preset は migration で seed (b) 設定ファイル | **(a)** | 管理者の独自許諾の追加・新版・廃止を実行時に行える。行の不変性を DB で保証する（UPDATE は `selectable` のみ） |

## 8. 他 unit との合意事項

| 事項 | 相手 | 内容 |
| --- | --- | --- |
| センシティブ分類の語彙（ID・名称・外部分類との対応） | ShuttlePub（D10 契約） | 初期セットはたたき台（性的表現・暴力・流血・虫・ネタバレ）。ID は仮。確定後に Booskiff が seed |
| 他者向け投稿の判定と固定の呼び出し順 | ShuttlePub | 本設計の `reserve`→永続化→`commit`/`release`。`reservation_id` の保存・復旧処理 |
| 添付スナップショットと `applied_revision`・失敗分の再試行 | ShuttlePub | §5 の方式。`missing`（削除）の扱い |
| タグ・センシティブ・許諾の連合表現 | ShuttlePub / Emumet | タグは ActivityPub 拡張として送信（d11-q03）。独自許諾の表現・非対応 peer の fallback は Booskiff では決めない |
| backfill の実行と許諾書込みの有効化順序 | ShuttlePub | 共有済み file ID の収集・backfill 呼び出し・突合（state-machine §6） |
| サービス JWT の発行元・issuer・`aud`・scope | ShuttlePub / Booskiff 運用 | T1 |
| ShuttlePub 側の API 契約 | ShuttlePub（別 unit） | WebAPI 定義のみ・utoipa・501 スタブの方針 |

## 9. D11 反映範囲は案 B で確定

**確定済み（ユーザー本人の回答「D11: bで確定しよう」）**: 画像のタグ・センシティブ分類を直したとき、その file を使う**同じ Account の全 Profile の投稿**に反映する（案 B）。
本文・本文分類・同一 bytes の別 file・他人の投稿は変更しない（d11-q01）。他 Account による同一 file の再利用は契約上起きない（`reserve` は所有者のみ。Booskiff は他人の file を 404 にする）。

### 本設計への反映

- Booskiff の契約は file 単位で、Profile を知らない（Profile 概念なし）。変更は file ごとに 1 件の通知になり、投稿の起点 Profile を示す項目（`change_context`）は**持たない**。
- ShuttlePub は `file_id` を使う同じ Account の全添付を対象に、添付ごとの `applied_revision` で反映済みかを管理する。Profile ごとの絞り込み・対象外投稿の参照版固定は不要。
- 失敗分の再試行は、対象となる全添付のうち失敗したものだけ（d11-q04）。
- テストベクトルは [test-vectors.md §11（G）](test-vectors.md#11-d11-反映範囲g)。

### 不採用の案 A との比較（記録）

案 A は「同じ Profile の投稿のみ反映し、対象外の投稿は参照版（変更前）のまま固定」する案で、packet 作成時の推奨だった。ユーザーが案 B を選んだため採用しない。比較は次のとおり。

| 依存箇所 | 案 B（確定） | 案 A（不採用） |
| --- | --- | --- |
| Booskiff の API 形 | file 単位。`change_context` なし | PATCH に任意の `change_context` を追加し、フィードに echo する必要があった |
| ShuttlePub の添付スナップショット | `applied_revision` で足りる。同じ Account の全添付を更新 | 対象外の添付は変更前のタグ・分類をスナップショットとして保持し続ける必要があった（Booskiff は過去の版を保持しない） |
| 失敗分の再試行の対象 | 失敗した添付 | 失敗した対象内の添付のみ |
| ShuttlePub 側で必要な情報 | Account → Profile の対応は不要 | 変更の起点の Profile と添付の Profile |

状態遷移表（[state-machine.md](state-machine.md) §2・§3）は両案で同形だったため、案 B の確定による変更はない。
ADR が必要かは closeout で判断する（本 unit では起票しない）。

## 10. 後続実装 unit への分割案と依存順序

| 順 | unit（案） | 内容 | 依存 |
| --- | --- | --- | --- |
| 1 | `media-metadata-license-catalog` | `license_definitions`・8 preset の seed・不変性の保証・管理 API（独自許諾・新版・廃止）・一覧/版取得 API | なし |
| 2 | `media-metadata-core` | `file_media_metadata`・revision・所有者向け GET/PATCH（タグ・分類）・`sensitive_categories` の seed・`reason` 付きエラー | なし（分類語彙は他 unit と合意後に seed） |
| 3 | `media-metadata-license-selection` | PATCH で許諾の版参照を設定・固定済みの拒否・書込みの有効化フラグ | 1・2 |
| 4 | `media-metadata-share-freeze` | サービス JWT（T1）・`reserve`/`commit`/`release`・失効・`DELETE` の整合・`batch-get`・**backfill-fixed** | 3 |
| 5 | `media-metadata-change-feed` | outbox・フィード API・保持期間 | 2（固定済み判定は 4） |
| 6 | `media-metadata-tag-suggestions` | 補完候補（own／共有済み 3 人以上）・集計 | 2（共有済みの判定は 4） |

- ShuttlePub 側の契約 unit（`reserve`→`commit` の呼び出し・添付スナップショット・フィードの購読・backfill 実行）は別 repo。順序は 4 の API 契約確定後、backfill 完了後に 3 の許諾書込みを有効化する。
- frontends の UI（許諾選択・固定表示・タグ入力・分類選択）は 2〜3 の API 確定後。
- 各 unit で OpenAPI（`openapi.json`）を再生成し、openapi-drift の CI を通す。本 unit では変更しない。

## 11. 根拠コードと決定済み事項の出典

- 認証・認可: `core/src/auth/extractor.rs`（`AccountContext`）、`core/src/auth/jwt.rs`（信頼 issuer の JWT 検証）、`core/src/auth/admin_auth.rs`（`AdminPrincipal`）。
- 所有者以外の 404: `core/src/drive/files.rs`（`load_owned_file`）。エラー形式: `core/src/error.rs`（`code`: unauthorized / forbidden / not_found / conflict / validation / rate_limited）。
- 公開・キャッシュ: `core/src/public.rs`、[drive/public-access.md](../drive/public-access.md)、[drive/caching.md](../drive/caching.md)。`publish_file` は publish ごとに乱数 `public_key` を発行し、unpublish は外部キャッシュを失効させない。
- migration の最新は `00008_folder_hierarchy.sql`（後続 unit が `00009` 以降を追加する）。`files` は `owner_type`/`owner_id`/`public_key`/`is_public` を持ち、メタデータ列を持たない。
- 決定済み事項の出典（host repo の intent）: grill 記録 d02-q01〜q11（file 共通・8 選択肢・初期値未指定・公開後固定・初回他者共有で固定・Booskiff 保存・既存画像は未指定／未共有のみ後設定・所有者のみ・同一 bytes は別 file・管理者の独自許諾）、d10（管理側が用意する共通分類）、d11-q01〜q04（自分の他投稿への反映・補完は自分優先→全体・タグは AP 拡張・失敗分だけ再試行）。
