# Booskiff ドキュメント索引

- [operations.md](operations.md): drive-foundation レビューで確定した運用仕様の要約メモ。
- Drive 関連の仕様詳細:
  - [drive/public-access.md](drive/public-access.md): `/public/{key}` 無認証公開エンドポイントの仕様。
  - [drive/caching.md](drive/caching.md): 公開コンテンツの immutable キャッシュと unpublish 時の挙動。
  - [drive/rate-limiting.md](drive/rate-limiting.md): プロセス内レート制限の仕様とマルチレプリカ caveat。

記述は実装 (core/src) を根拠とし、コード変更時は対応するドキュメントも更新すること。
