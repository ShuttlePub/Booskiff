# Booskiff ドキュメント索引

- [operations.md](operations.md): drive-foundation レビューで確定した運用仕様の要約メモ。
- Drive 関連の仕様詳細:
  - [drive/public-access.md](drive/public-access.md): `/public/{key}` 無認証公開エンドポイントの仕様。
  - [drive/caching.md](drive/caching.md): 公開コンテンツの immutable キャッシュと unpublish 時の挙動。
  - [drive/rate-limiting.md](drive/rate-limiting.md): プロセス内レート制限の仕様とマルチレプリカ caveat。
- 画像メタデータ（利用許諾・通常タグ・センシティブ分類）の producer 契約設計（**設計のみ・未実装**）:
  - [media-metadata/README.md](media-metadata/README.md): モデル・固定境界・責務分担・技術判断 T1〜T11・D11 反映範囲（案 B で確定）・後続 unit 分割案。
  - [media-metadata/api.md](media-metadata/api.md): 所有者向け・ShuttlePub 向け内部・管理者向け API の型・route・エラー。
  - [media-metadata/state-machine.md](media-metadata/state-machine.md): 許諾の固定状態・予約の遷移表、既存画像の状態例 E1〜E6 と backfill 手順。
  - [media-metadata/test-vectors.md](media-metadata/test-vectors.md): 認可・競合・失敗を含むテストベクトル。

記述は実装 (core/src) を根拠とし、コード変更時は対応するドキュメントも更新すること。
