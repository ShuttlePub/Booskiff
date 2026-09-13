# 公開コンテンツのキャッシュ仕様

`/public/{key}` のレスポンスに対するキャッシュ semantics と、unpublish・再 publish 時の挙動。実装根拠は `core/src/public.rs` と `core/src/drive/files.rs`。

## 発行される Cache-Control

- 公開レスポンスには固定で `Cache-Control: public, max-age=31536000, immutable` を返す (`public.rs` の `IMMUTABLE_CACHE_CONTROL`)。
- `public` により共有キャッシュ (CDN 等) も保存可能、`max-age=31536000` (1 年) + `immutable` でブラウザは再検証なしに使い回す。

## URL キーの性質

- `publish_file` は publish のたびに 32 バイト乱数を base64url エンコードした新しい `public_key` を発行する (`files.rs`)。再 publish のたびに URL が変わる。
- キーはコンテンツハッシュではなく乱数。ただし同一 `public_key` が後から別コンテンツを指す経路は現状存在しない (アップロードは常に新規ファイル・新規オブジェクトを作成し、既存ファイルのコンテンツを差し替えるエンドポイントはない)。このため immutable 前提と矛盾しない。

## unpublish / 削除しても外部キャッシュは失効しない

- `unpublish_file` は `is_public = FALSE, public_key = NULL` に更新するだけ。以後その URL へのリクエストは core で 404 になる。
- core は発行済みの Cache-Control を取り消す手段を持たない。ブラウザや CDN 等の外部キャッシュに入ったコンテンツは最長 1 年残り得る。
- ファイル削除も同様で、外部キャッシュの即時失効は一切保証しない。

## 運用含意

- unpublish は「これ以上 core から配信しない」という意味であり、「発行済み URL や外部キャッシュの失効」ではない。漏洩した公開キーの拡散阻止を unpublish に頼らないこと。
- CDN を前置している場合は CDN 側の purge を別途実施する必要がある。
- 漏洩が許容できないコンテンツは公開エンドポイントに出さず、認証付き `download-url` (presigned URL、TTL 既定 900 秒、`BOOSKIFF_PRESIGNED_GET_TTL_SECS`) で配布する。
- コンテンツを更新したい場合は新規アップロード + publish で新しい URL を発行し、旧 URL は unpublish で 404 にする運用を想定している。
