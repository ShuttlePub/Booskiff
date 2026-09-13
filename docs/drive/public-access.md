# 公開アクセス仕様 (`/public/{key}`)

`GET /public/{key}` は無認証の公開ダウンロードエンドポイント。実装は `core/src/public.rs`、ルータの組み込みは `core/src/main.rs` (`public::public_router()` の merge)。

## 認証・認可

- Bearer トークンを要求しない。認証ミドルウェア (`AccountContext` 抽出器) を一切通らないルートとして merge されている (`main.rs`)。
- owner 一致や権限チェックも行わない。公開鍵 (`{key}`) を知っていれば誰でもダウンロードできる、capability URL モデル。
- 帯域計量 (転送量の計測・課金) は行わない。初期スコープ外。

## 何を返すか

1. `files.public_key = {key} AND is_public = TRUE` に一致する行を DB から引く (`public.rs` `get_public_file`)。一致しなければ 404 (キー不明・非公開の区別はレスポンスから判別できない)。
2. そのファイルの original オブジェクト (`object_kind = 'original'`) の `storage_key` を引き、S3 互換ストレージからストリーミングで返す。
3. レスポンスヘッダ:
   - `Content-Type` / `Content-Length`: S3 オブジェクト由来の値をそのまま引き継ぐ。
   - `Cache-Control: public, max-age=31536000, immutable` (固定値。詳細は [caching.md](caching.md))。

## 意図的に要求しないもの

- 認証 (上記)。
- owner ごとのレート制限 (認証済み API とは別系統。下記)。
- 帯域計量・課金判定。

## DoS 緩和 (per-IP レート制限)

- DB 参照の前に `PublicRateLimiter` で per-IP チェックを行い、超過時は 429 `rate_limited`。
- 既定 300 req/min。`BOOSKIFF_PUBLIC_RATE_LIMIT_RPM` で変更。
- 制限キーの決定: 既定は接続元 peer IP。`BOOSKIFF_TRUST_PROXY_HEADERS=true` の場合は X-Forwarded-For のカンマ区切りリスト中、最左の有効 IP を使う (解釈不能・欠落時は peer IP にフォールバック)。どちらからも IP を取れない場合は制限をスキップする (fail-open)。
- `BOOSKIFF_TRUST_PROXY_HEADERS` は X-Forwarded-For を浄化する信頼プロキシ (同梱 Caddy 等) の背後でのみ有効化すること。直接公開時に有効化するとヘッダー偽装で制限を回避できる。セルフホスト用 Compose (`deploy/self-hosting/compose.yml`) は Caddy 経由のみの公開のため既定 `true`、core 自体の既定は `false`。
- レート制限のマルチレプリカ caveat は [rate-limiting.md](rate-limiting.md) を参照。

## 運用上の注意

- 公開 URL を配布することはコンテンツを公開することと同義。key は推測困難な乱数だが、秘匿コンテンツには公開ではなく認証付き `download-url` (presigned URL、TTL 既定 900 秒) を使う。
- unpublish 時のキャッシュ挙動は [caching.md](caching.md) を参照。
