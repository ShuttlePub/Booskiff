# 運用仕様メモ

drive-foundation レビューで確定した運用上の仕様ポイントをまとめる。実装の根拠は core/src 配下の各モジュール (`public.rs`, `state.rs`, `auth/rate_limit.rs`, `billing/cache.rs`, `drive/files.rs`, `drive/upload_body.rs`, `config.rs`)。

## `/public/{key}` は無認証の公開エンドポイント

- 認証なしで誰でもダウンロードできる。帯域計量 (レスポンス転送量の計測・課金) は初期スコープ外。
- DoS 緩和として per-IP レート制限を付与している。既定 300 req/min で、環境変数 `BOOSKIFF_PUBLIC_RATE_LIMIT_RPM` で変更できる。
- 既定の制限キーは接続元の peer IP (`ConnectInfo<SocketAddr>`)。`BOOSKIFF_TRUST_PROXY_HEADERS` は既定 `false` で、X-Forwarded-For は無視する。
- リバースプロキシ配下では peer IP がプロキシの IP になるため、`BOOSKIFF_TRUST_PROXY_HEADERS=true` で X-Forwarded-For のカンマ区切りリスト中、最左の有効な IP を制限キーに使う。ヘッダーがない場合や有効な IP を解析できない場合は peer IP に戻る。どちらからも IP を取得できない環境では従来どおり制限をスキップする (fail-open)。
- この設定は X-Forwarded-For を適切に設定・浄化する信頼できるプロキシ (同梱 Caddy 等) の背後でのみ有効化すること。core を直接公開した状態で有効化すると、ヘッダーの偽装により制限を回避できる。core への直接アクセスを許可しないこと。
- セルフホスト用 Compose は Caddy だけがホストにポートを公開し、core は内部ネットワークのみのため、この Compose に限り `BOOSKIFF_TRUST_PROXY_HEADERS` の既定値を `true` にしている。core を直接公開する構成へ変更する場合は `false` にする。

## 公開ファイルのキャッシュと失効

- 公開レスポンスには `Cache-Control: public, max-age=31536000, immutable` を返す。
- unpublish / 削除しても、ブラウザや CDN 等の外部キャッシュにコンテンツが残り得る。外部キャッシュの即時失効は保証しない。
- 緩和の仕組み: `publish_file` は publish のたびに public_key を乱数で再生成する。unpublish で public_key は NULL になるため旧キーの URL は 404 になり、再 publish すると別キーの URL に変わる。

## レート制限 state と課金キャッシュはプロセス内メモリ

- owner 用 `RateLimiters`、public 用 `PublicRateLimiter`、課金解決キャッシュ `BillingCache` はすべてプロセス内メモリに保持し、複数レプリカ間で共有されない。レプリカごとに実効制限 (req/min やキャッシュ鮮度) が発散し得る。
- rpm が変わると対象キーの limiter を再生成するため、消費済み分はリセットされ burst が復活する。既存トークンバケットの残量を新 quota と調停するコストを避ける、意図的な設計上の制約。
- `BillingCache` の TTL は既定 60 秒。環境変数 `BOOSKIFF_BILLING_CACHE_TTL_SECS` で変更でき、0 を指定するとキャッシュ自体を無効化 (保存をスキップ) する。管理者 API による書き込み (課金ルール・割当・プランの変更) 時は、対象 owner のエントリまたは全体を即時無効化する。

## ボディサイズ強制

- アップロードのボディサイズ強制は `CountingBody` (`drive/upload_body.rs`) が唯一の経路。`upload_file` が axum の `Body` を直接抽出するため `DefaultBodyLimit` レイヤは no-op になり、撤去済み。
- 実測バイト数 (counted) が `max_file_bytes` を超えた場合は 413 `payload_too_large`。宣言 Content-Length を超えた場合は 400 `validation`。

## 環境変数

| 変数 | 既定値 | 意味 |
| --- | --- | --- |
| `BOOSKIFF_PUBLIC_RATE_LIMIT_RPM` | 300 | `/public/{key}` の per-IP レート制限 (req/min) |
| `BOOSKIFF_TRUST_PROXY_HEADERS` | false (セルフホスト Compose は true) | 信頼プロキシの X-Forwarded-For を制限キーに利用。直接公開時は有効化禁止 |
| `BOOSKIFF_BILLING_CACHE_TTL_SECS` | 60 | 課金解決キャッシュの TTL (秒)。0 で無効 |
