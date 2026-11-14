# 画像メタデータ API 案

> **設計文書（未実装）**。ここに書く route・型・SQL・JSON は案であり、稼働コード・migration・`openapi.json` は本 unit で追加・変更しない。
> コード例は後続 unit が utoipa で定義するときの出発点で、`core/src` には存在しない。
> 全体像は [README.md](README.md)、状態遷移は [state-machine.md](state-machine.md)、検証項目は [test-vectors.md](test-vectors.md)。

既存実装との整合の根拠: Account 認証は `core/src/auth/extractor.rs` の `AccountContext`（Bearer JWT・信頼 issuer・owner claim）、
エラー本文は `core/src/error.rs` の `{"error":{"code","message"}}`、所有者以外への 404 は `core/src/drive/files.rs` の `load_owned_file`、
管理者は `core/src/auth/admin_auth.rs` の `AdminPrincipal`。

## 1. 共通規約

| 項目 | 規約 |
| --- | --- |
| ベース | 所有者向けは `/v1/...`、ShuttlePub 向け内部は `/v1/internal/media-metadata/...`、管理者向けは `/v1/admin/media-metadata/...` |
| 形式 | JSON（UTF-8）。時刻は RFC 3339。ID は UUID（file・reservation）または文字列 slug（license・category） |
| 所有者向けの認証 | 既存の Account JWT（`AccountContext`）。所有者は JWT から決まる。他人の file は **404**（存在秘匿。不在と区別しない） |
| 内部 API の認証 | ShuttlePub のサービス JWT（T1）。Account JWT では呼べず（403）、サービス JWT では所有者向け route を呼べない（401/403） |
| 管理者の認証 | 既存の admin token（`AdminPrincipal`） |
| キャッシュ | メタデータ系 route は **`Cache-Control: no-store`**。`/public/{key}` の immutable キャッシュを流用しない（README §6） |
| レート制限 | 所有者向けは既存の owner 単位制限（`AccountContext`）。内部 API はサービス単位の別制限（後続 unit で値を決める） |
| エラー本文 | 既存の `{"error":{"code","message"}}` を維持。機械判別用に**追加フィールド `reason`（任意）**を足す提案（T10）。既存クライアントは未知フィールドを無視できる |

### エラー対応表

| HTTP | `code`（既存） | `reason`（提案） | 主な発生条件 |
| --- | --- | --- | --- |
| 400 | `validation` | `invalid_tag` / `too_many_tags` / `unknown_license` / `license_not_selectable` / `unknown_sensitive_category` / `category_retired` / `invalid_request` / `batch_too_large` / `invalid_ttl` | 入力検証 |
| 401 | `unauthorized` | — | token 不備・未信頼 issuer |
| 403 | `forbidden` | `missing_scope` / `license_write_disabled` | 内部 API にサービス scope が無い／許諾の書込みが無効な運用段階（state-machine §6） |
| 404 | `not_found` | — | file/reservation が無い、または所有者不一致（区別しない） |
| 409 | `conflict` | `revision_mismatch` / `license_fixed` / `license_reserved` / `file_reserved` / `idempotency_key_reused` / `reservation_lapsed` / `reservation_released` / `reservation_committed` / `file_missing` / `cursor_expired` / `license_exists` | 競合・状態違反 |
| 429 | `rate_limited` | — | レート制限 |

## 2. データモデル案

### 2.1 型（Rust / utoipa 案）

```rust
/// file 単位のメタデータ。行が無い file も同じ形で返す（revision 0・未指定・未固定）。
pub struct MediaMetadata {
    pub file_id: Uuid,
    pub revision: i64,                 // 行なし = 0。実際に値が変わったときだけ +1
    pub license: LicenseStatus,
    pub tags: Vec<String>,             // 正規化済み。順序は保持
    pub sensitive_categories: Vec<String>, // category id。昇順・重複なし
    pub updated_at: Option<String>,    // 行なしは null
}

pub struct LicenseStatus {
    pub id: String,                    // 例 "cc-by-4.0" / "unspecified" / "x-studio-private"
    pub version: i32,                  // カタログ行の版（1 始まり）
    pub state: LicenseFixState,
    pub fixed_at: Option<String>,
}

pub enum LicenseFixState { Unfixed, Reserved, Fixed }   // "unfixed" | "reserved" | "fixed"

pub struct LicenseRef { pub id: String, pub version: Option<i32> }

pub struct PatchMediaMetadataRequest {
    pub expected_revision: i64,                    // 必須
    pub license: Option<LicenseRef>,               // 省略 = 変更しない
    pub tags: Option<Vec<String>>,                 // 省略 = 変更しない。指定は全置換
    pub sensitive_categories: Option<Vec<String>>, // 同上
    // D11 は案 B で確定（README §9）。Profile を示す change_context は持たない。
}
```

### 2.2 保存（SQL 案。migration ではない）

```sql
-- 行なし = 「未指定・未固定・revision 0・タグなし・分類なし」。既存 file に行を作らない。
CREATE TABLE file_media_metadata (
    file_id UUID PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision >= 1),
    license_id TEXT NOT NULL DEFAULT 'unspecified',
    license_version INT NOT NULL DEFAULT 1,
    license_state TEXT NOT NULL DEFAULT 'unfixed'
        CHECK (license_state IN ('unfixed','reserved','fixed')),
    reservation_id UUID NULL,                -- license_state = 'reserved' のときのみ
    fixed_at TIMESTAMPTZ NULL,
    fixed_via TEXT NULL CHECK (fixed_via IN ('share','backfill')),
    tags TEXT[] NOT NULL DEFAULT '{}',
    sensitive_categories TEXT[] NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (license_id, license_version) REFERENCES license_definitions(license_id, version)
);

CREATE TABLE license_definitions (        -- 行は不変。変更は新版の追加
    license_id TEXT NOT NULL,
    version INT NOT NULL CHECK (version >= 1),
    kind TEXT NOT NULL CHECK (kind IN ('preset','custom')),
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    url TEXT NULL,
    selectable BOOLEAN NOT NULL DEFAULT TRUE,   -- 唯一の可変列。廃止は FALSE にするだけ
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (license_id, version)
);

CREATE TABLE sensitive_categories (
    id TEXT PRIMARY KEY,                   -- 固定 ID（slug）
    label TEXT NOT NULL,
    retired BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE media_license_reservations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    idempotency_key UUID NOT NULL,
    request_hash BYTEA NOT NULL,           -- file_ids と ttl の正規化ハッシュ（再送の同一性判定）
    status TEXT NOT NULL CHECK (status IN ('active','committed','released','lapsed')),
    post_ref TEXT NULL,                    -- ShuttlePub の不透明な参照（監査用）
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ NULL,
    UNIQUE (owner_type, owner_id, idempotency_key)
);

CREATE TABLE media_license_reservation_items (
    reservation_id UUID NOT NULL REFERENCES media_license_reservations(id) ON DELETE CASCADE,
    file_id UUID NOT NULL,                 -- files への FK は張らない（file 削除後も予約の履歴を残す）
    already_fixed BOOLEAN NOT NULL,
    PRIMARY KEY (reservation_id, file_id)
);

CREATE TABLE media_metadata_changes (      -- 変更フィードの outbox
    seq BIGSERIAL PRIMARY KEY,
    txid XID8 NOT NULL DEFAULT pg_current_xact_id(),
    file_id UUID NOT NULL,                 -- FK なし（deleted 通知のため）
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    revision BIGINT NOT NULL,
    kinds TEXT[] NOT NULL,                 -- 'tags' | 'sensitive_categories' | 'deleted'
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

- 所有者は `files.owner_type` / `files.owner_id` が正で、メタデータ行に重複して持たない。
- `files` の既存列・既存 route の応答（`FileResponse`）は変えない。メタデータは `/v1/files/{id}/metadata` の別リソースにする。
  これにより `openapi.json` の既存スキーマは変わらず、画像 bytes・公開 URL・`/public/{key}` の挙動にも影響しない。
- 許諾の `unspecified` は**組み込みの preset**（`license_id = 'unspecified'`, `version = 1`）として `license_definitions` に seed する。
  API は常に `license.id` を返し、`null` を返さない。

### 2.3 許諾カタログ（8 選択肢＋管理者独自）

| `license_id` | `kind` | `title` | 公式リンク（`url`） |
| --- | --- | --- | --- |
| `cc-by-4.0` | preset | CC BY 4.0 | https://creativecommons.org/licenses/by/4.0/ |
| `cc-by-sa-4.0` | preset | CC BY-SA 4.0 | https://creativecommons.org/licenses/by-sa/4.0/ |
| `cc-by-nd-4.0` | preset | CC BY-ND 4.0 | https://creativecommons.org/licenses/by-nd/4.0/ |
| `cc-by-nc-4.0` | preset | CC BY-NC 4.0 | https://creativecommons.org/licenses/by-nc/4.0/ |
| `cc-by-nc-sa-4.0` | preset | CC BY-NC-SA 4.0 | https://creativecommons.org/licenses/by-nc-sa/4.0/ |
| `cc-by-nc-nd-4.0` | preset | CC BY-NC-ND 4.0 | https://creativecommons.org/licenses/by-nc-nd/4.0/ |
| `cc0-1.0` | preset | CC0 1.0（6 種のライセンスとは別の、法の許す範囲での権利放棄ツール） | https://creativecommons.org/publicdomain/zero/1.0/ |
| `unspecified` | preset | 未指定（追加の利用許諾を示さない。再利用の全面禁止でも既存許諾の取消しでもない） | なし |
| `x-<slug>` | custom | 管理者が定義（`^x-[a-z0-9][a-z0-9-]{0,47}$`） | 任意（https のみ） |

- 8 つの preset は migration で seed する（版 1）。CC の「4.0」は `license_id` に含め、`version` は**カタログ行の版**（表示文言・リンクの定義版）で、CC 自体のバージョンではない。
- 未指定と CC0 は同じ意味にしない。`unspecified` は初期値で、選んだときだけ CC 等を付ける。
- `summary` は閲覧者に出す短い説明文（~1000 文字以内）。独自許諾の本文・URL を**一般ユーザーが入力する**機能は初回外（d02-q02）。管理者定義の独自許諾だけを選べる。
- 管理者が定義を変えるときは**新しい版の行を追加**する。既存行（`title`/`summary`/`url`）は更新しない。固定済み file はこれまでの `(id, version)` を指し続けるので、条件は変わらない。
- 廃止は `selectable = FALSE` にする。新しい選択はできなくなるが、既に参照している file（固定済み・未固定とも）の参照は有効なまま。
- 独自許諾の連合上の表現（`license` URL を使うか、ShuttlePub 独自の拡張か）は Booskiff では決めず、ShuttlePub/Emumet 側の契約と合意する（T11 の外・他 unit 合意事項）。

### 2.4 センシティブ分類

- Booskiff が語彙の正本を持つ（T2。ShuttlePub の D10 契約との合意が必要）。固定 ID（slug）と表示名を持ち、初期セットのたたき台は
  `sexual`（性的表現）・`violence-gore`（暴力・流血）・`insects`（虫）・`spoiler`（ネタバレ）。**ID と名称は仮**で、d10-q08 の「正式名称・固定 ID・外部分類との対応」の確定後に seed する。
- file には 0 個以上の分類 ID が付く。画像単位。本文の分類・投稿 CW・外部の二値センシティブ指定は ShuttlePub の責務（Booskiff は扱わない）。
- 退役（`retired = TRUE`）した分類は新しく付けられないが、既に付いている file からは外れない。追加・改名・廃止の管理 API は初回外（初期セットは migration で seed）。

### 2.5 タグ

- 正規化（T9）: Unicode NFKC → 前後の空白除去 → 内部の連続空白を 1 つにまとめる → 先頭の `#` を除去 → 小文字化（case fold）。正規化後に空になれば 400 `invalid_tag`。
- 長さ上限: 正規化後 64 文字（Unicode スカラー値）。制御文字・`,` 改行を含むものは 400。
- 1 file あたり最大 30 個。正規化後の重複は先頭を残して除く。順序は保持する。
- 「変更なし」の判定は正規化後の配列を順序込みで比較する。

## 3. 所有者向け API

### 3.1 `GET /v1/files/{id}/metadata`

自分の file のメタデータを返す。行が無ければ既定値（revision 0）。他人の file・不在は 404。

```json
{
  "file_id": "6f1c0a52-3a7e-4a6e-8c0a-0d9a6f6b8f10",
  "revision": 3,
  "license": { "id": "cc-by-4.0", "version": 1, "state": "fixed", "fixed_at": "2026-11-20T09:14:03Z" },
  "tags": ["風景", "夕焼け"],
  "sensitive_categories": ["insects"],
  "updated_at": "2026-11-21T01:02:03Z"
}
```

行なし（既存画像の導入直後）:

```json
{
  "file_id": "6f1c0a52-3a7e-4a6e-8c0a-0d9a6f6b8f10",
  "revision": 0,
  "license": { "id": "unspecified", "version": 1, "state": "unfixed", "fixed_at": null },
  "tags": [],
  "sensitive_categories": [],
  "updated_at": null
}
```

### 3.2 `PATCH /v1/files/{id}/metadata`

merge-patch 風。省略したフィールドは変更しない。`tags`・`sensitive_categories` を指定すると全置換。

```json
{ "expected_revision": 3, "tags": ["風景", "夕焼け", "海"], "license": { "id": "cc-by-4.0" } }
```

処理順（原子的。いずれかで失敗したらどのフィールドも適用しない）:

1. 認証・file の所有確認（不一致は 404）。
2. 本文の検証（`expected_revision` は 0 以上で必須、タグ・分類・許諾の形式。失敗は 400）。
3. `files` 行を `FOR UPDATE` でロック。
4. `expected_revision` と現在の revision（行なしは 0）を比較。不一致は 409 `revision_mismatch`。
5. 許諾の判定:
   - `license` 省略、または指定が現状の `(id, version)` と同一 → 許諾は変更なし。
   - `version` 省略は、その `license_id` の**選択可能な最新版**に解決する（保存は明示の版）。解決後が現状と同一なら変更なし。
   - 異なる値を指定し、`state = fixed` → 409 `license_fixed`、`state = reserved` かつ予約が未失効 → 409 `license_reserved`
     （予約が TTL 超過なら予約を `lapsed` にして続行。state-machine T08）。
   - 存在しない `(id, version)` は 400 `unknown_license`、`selectable = FALSE` の版を新しく選ぶのは 400 `license_not_selectable`。
   - 許諾の書込みが無効な運用段階（backfill 前。state-machine §6）は 409 `license_fixed` ではなく 403 `forbidden`（`reason: license_write_disabled`）とする。
6. 分類の判定: 未知の ID は 400 `unknown_sensitive_category`。`retired` の ID を**新しく追加**するのは 400 `category_retired`（現状に既にあるものを残すのは可）。
7. 実際に値が変わるか判定。変わらなければ DB を更新せず **revision を進めず** 200 で現状を返す（行なし file の no-op は行も作らない）。
8. 変わる場合: 行を作成（revision 1）または更新（revision +1）。`fixed` の file でタグ・分類が変わったときは同一トランザクションで `media_metadata_changes` に追加。

成功は 200 と更新後の `MediaMetadata`。許諾が固定されていても、タグ・分類だけの PATCH は成功する。

### 3.3 `GET /v1/media-metadata/licenses`

選択可能な許諾（`selectable = TRUE` の最新版）の一覧。`unspecified` を含む。

```json
{ "items": [
  { "id": "cc-by-4.0", "version": 1, "kind": "preset", "title": "CC BY 4.0",
    "summary": "表示を条件に…", "url": "https://creativecommons.org/licenses/by/4.0/" }
] }
```

### 3.4 `GET /v1/media-metadata/licenses/{id}/versions/{version}`

指定版の定義（廃止された版や固定済み file が参照する版も取得できる）。本文は不変（`selectable` は返さない）なので、
`Cache-Control: private, max-age=3600` を付けてよい（認証付きのため `public` は付けない。`immutable` は付けない）。
存在しなければ 404。ShuttlePub 用の同等 route は §4.5。

### 3.5 `GET /v1/media-metadata/sensitive-categories`

`retired` でない分類の一覧。`include_retired=true` で退役済みも返す（既に付いた分類の表示名解決用）。

```json
{ "items": [ { "id": "sexual", "label": "性的表現", "retired": false } ] }
```

### 3.6 `GET /v1/media-metadata/tag-suggestions?prefix=<s>&limit=<n>`

タグの補完候補。`prefix` は正規化してから前方一致（1 文字以上）。`limit` 既定 10・最大 20。自分のタグを優先し、その後にサーバー全体のタグを返す（d11-q02）。

```json
{ "own": ["風景", "夕焼け"], "shared": ["風景写真"] }
```

- `own`: 自分が所有する file に付いているタグ（固定前を含む）。使用回数の降順、同数は最近使用順。
- `shared`: 他者向けに共有済み（`fixed`）の file に付いているタグのうち、**3 人以上の異なる所有者**が使っているもの（T3）。`own` に含まれるものは除く。
  未共有の file のタグや、使用者が少ないタグは、他人の補完に出さない（個人的なタグの漏洩防止）。件数や所有者数は返さない。
- 集計は定期更新でよい（結果整合）。補完の欠落・遅延は正しさに影響しない。

## 4. ShuttlePub 向け内部 API

認証: サービス JWT（T1）。`iss` は信頼 issuer、`aud` は Booskiff、`scope` claim に `media-metadata:service` を含む。**owner claim を持たない**ので
`AccountContext` を通る所有者向け route は使えない。所有者向け Account JWT で内部 route を呼ぶと 403（`missing_scope`）。
リクエスト本文の `owner`（`{owner_type, owner_id}`）は「その投稿の所有 Account」を ShuttlePub が主張する値で、Booskiff は file の実際の所有者と一致する場合のみ処理する（不一致は 404）。

### 4.1 `POST /v1/internal/media-metadata/reservations`（予約）

```json
{
  "owner": { "owner_type": "account", "owner_id": "acc_123" },
  "idempotency_key": "0b6f5e0e-7d9c-4a55-9d0a-5e77b3b0c001",
  "file_ids": ["6f1c0a52-…", "7a2d1b63-…"],
  "ttl_seconds": 3600,
  "post_ref": "post-draft-8841"
}
```

- `file_ids` は 1〜20 件（重複は 400）。`ttl_seconds` は省略時 3600、1〜86400 の範囲外は 400 `invalid_ttl`。
- 全件の所有確認 → 全件を ID 昇順で `FOR UPDATE` → 判定 → 反映を**原子的**に行う。1 件でも所有者不一致・不在なら全体が 404（どの file かは区別しない）。
- 判定: `unfixed` → `reserved` にして `reservation_id` を設定。`reserved` かつ別の未失効の予約 → 全体が 409 `file_reserved`。
  `reserved` かつ予約が TTL 超過 → 旧予約を `lapsed` にして新予約で `reserved`。`fixed` → 変化なし（`already_fixed: true`）。
- 新規は 201、同じ `(owner, idempotency_key)` で同一内容の再送は 200（現在の `status` と最新スナップショット）。内容が異なる再送は 409 `idempotency_key_reused`。

```json
{
  "reservation_id": "c1a8…",
  "status": "active",
  "expires_at": "2026-11-20T10:14:03Z",
  "items": [
    { "file_id": "6f1c0a52-…", "already_fixed": false, "metadata": { "file_id": "6f1c0a52-…", "revision": 2, "license": { "id": "cc-by-4.0", "version": 1, "state": "reserved", "fixed_at": null }, "tags": ["風景"], "sensitive_categories": [], "updated_at": "…" } }
  ]
}
```

### 4.2 `POST /v1/internal/media-metadata/reservations/{reservation_id}/commit`（確定）

本文 `{ "owner": {…} }`。予約内の全 file を原子的に `fixed`（`fixed_at`・`fixed_via = 'share'`、revision +1）にする。
`already_fixed` の項目は変化なし。応答は `status: "committed"` と確定後の `items`（`metadata` の `license.state = "fixed"`）。
状態ごとの応答は state-machine §3（R04〜R08）: 再送は 200（冪等）、`lapsed` は 409 `reservation_lapsed`、`released` は 409 `reservation_released`、
予約内の file が削除済みなら 409 `file_missing`（何も確定しない）。所有者不一致・不在は 404。TTL 超過のみで `active` のままなら成功する（遅延失効）。

### 4.3 `POST /v1/internal/media-metadata/reservations/{reservation_id}/release`（解放）

本文 `{ "owner": {…} }`。`active` の予約を `released` にし、対象 file を `unfixed` に戻す（revision 変化なし）。
応答は `status: "released"`。`released`/`lapsed` への再送は 200（冪等）、`committed` は 409 `reservation_committed`。

### 4.4 `POST /v1/internal/media-metadata/batch-get`（一括取得）

```json
{ "file_ids": ["6f1c0a52-…", "9e3f2c74-…"] }
```

- 1〜100 件（超過は 400 `batch_too_large`）。
- 応答は、**`fixed`（他者向けに共有済み）の file だけ**のメタデータを `items` に、それ以外（未共有・予約中・削除済み・不在）を区別せず `missing` に返す。
  未共有 file のタグ・分類・許諾を ShuttlePub 経由で他者に見せないため。予約中の file は `reserve`/`commit` 応答で取得済みのはず。
- `items` には `owner` を含める（ShuttlePub が添付の所有者と照合できる）。

```json
{
  "items": [ { "owner": { "owner_type": "account", "owner_id": "acc_123" },
               "metadata": { "file_id": "6f1c0a52-…", "revision": 4, "license": { "id": "cc-by-4.0", "version": 1, "state": "fixed", "fixed_at": "…" }, "tags": ["風景"], "sensitive_categories": ["insects"], "updated_at": "…" } } ],
  "missing": ["9e3f2c74-…"]
}
```

- ShuttlePub は `missing` を「削除された、または共有を確認できない」として扱い、**最後に保持しているスナップショットを許諾の根拠として表示し続ける**（file 削除・unpublish は許諾の取消しではない）。

### 4.5 `GET /v1/internal/media-metadata/licenses/{id}/versions/{version}`

§3.4 と同じ本文。サービス JWT で呼べる。ShuttlePub は `(id, version)` をキーに本文を保持してよい（本文は不変）。

### 4.6 `GET /v1/internal/media-metadata/changes?after=<cursor>&limit=<n>`（変更フィード）

通知のみ。**真実は §4.4 の一括取得**。`limit` 既定 100・最大 500。`after` 省略は保持期間内の先頭から。

```json
{
  "items": [
    { "seq": "1042", "file_id": "6f1c0a52-…", "owner": { "owner_type": "account", "owner_id": "acc_123" },
      "revision": 4, "kinds": ["tags"], "changed_at": "…" }
  ],
  "next_cursor": "1042",
  "has_more": false
}
```

- 配信は at-least-once。同一 file の複数の変更は 1 件にまとめてよい（`revision` は最新値）。順序は保証しない（`revision` の大きい方が新しい）。
- コミット順の取りこぼしを避けるため、サーバーは `txid` が現在のスナップショットの xmin より小さい行だけを返す（outbox の標準的手法。T8）。
- 保持期間（既定 30 日）を過ぎた `after` は 409 `cursor_expired`。ShuttlePub は §4.4 で全添付を突合して回復する。
- フィードに出るのは `fixed` の file の `tags` / `sensitive_categories` の変更と `deleted`（state-machine §7）。
- 項目は file 単位。D11 は案 B（同じ Account の全 Profile に反映）で確定しているため、起点の Profile を示す項目は無い。ShuttlePub が `file_id` を使う同じ Account の全添付を対象にする。

### 4.7 `POST /v1/internal/media-metadata/backfill-fixed`（既存画像の移行用）

```json
{ "owner": { "owner_type": "account", "owner_id": "acc_123" }, "file_ids": ["…"], "dry_run": true }
```

- 1〜500 件。各 file を ID 昇順でロックし、`unfixed` を `fixed`（`fixed_via = 'backfill'`）にする。冪等。
- 結果: `fixed` / `already_fixed` / `not_found`（不在・所有者不一致を区別しない）/ `skipped_reserved`（予約中。0 件のはず）。`dry_run: true` は DB を変えず結果の見込みだけ返す。
- 行が無い file は `unspecified` v1・revision 1 の行を作る。行がある file は設定済みの許諾のまま固定する。フィードには出さない。
- 手順と順序の制約は state-machine §6。

## 5. 管理者向け API（許諾カタログ）

| route | 本文 | 動作 |
| --- | --- | --- |
| `POST /v1/admin/media-metadata/licenses` | `{ "license_id": "x-studio", "title", "summary", "url"? }` | 独自許諾を版 1 で追加。preset の ID・既存 ID は 409 `license_exists`。`license_id` 形式違反は 400 |
| `POST /v1/admin/media-metadata/licenses/{id}/versions` | `{ "title", "summary", "url"? }` | **新しい版**を追加（版は最大版 + 1）。既存版は変更しない。preset は対象外（preset の定義変更は migration で版を追加する） |
| `POST /v1/admin/media-metadata/licenses/{id}/deprecate` | `{ "version"? }` | 指定版（省略は全版）を `selectable = FALSE` にする。参照中の file には影響しない |

認可は既存の `AdminPrincipal`。管理者の操作は固定済み file の条件を変えない（新版の追加か選択可否の変更だけ）。
センシティブ分類の管理 API は初回外（運用・改名・廃止の方式が未決）。

## 6. 認可マトリクス

| 操作 | 所有者 JWT | サービス JWT | 管理者 token | 無認証 |
| --- | --- | --- | --- | --- |
| `GET/PATCH /v1/files/{id}/metadata` | 自分の file のみ（他人は 404） | 不可 | 不可 | 401 |
| `GET /v1/media-metadata/{licenses,sensitive-categories,tag-suggestions}` | 可 | 不可 | 不可 | 401 |
| `…/reservations`・`commit`・`release`・`backfill-fixed` | 403 | 本文の `owner` が file の所有者と一致する場合のみ（不一致は 404） | 不可 | 401 |
| `…/batch-get`・`…/changes` | 403 | 可（`batch-get` は `fixed` の file のみ中身を返す） | 不可 | 401 |
| `/v1/admin/media-metadata/…` | 不可 | 不可 | 可 | 401 |
| `/public/{key}` | メタデータを一切返さない・参照しない（既存挙動を変更しない） |||

## 7. 既存 API への影響

- `POST /v1/files`（upload）・`DELETE /v1/files/{id}`・`POST/DELETE /v1/files/{id}/publish`・`GET /v1/files` の**応答スキーマは変えない**。
- `DELETE /v1/files/{id}` には後続 unit で次を足す: `reserved`（未失効）の file は 409 `file_reserved`、`fixed` の file の削除時はメタデータ行の CASCADE 削除と `deleted` 通知。
  削除は許諾の取消しではない（ShuttlePub のスナップショットは残る）。
- `unpublish` は固定を解かない。メタデータの変更は `files` 行の列を更新しないので、画像 bytes・`public_key`・`/public/{key}` のレスポンスは変わらない。
