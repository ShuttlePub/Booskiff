# レート制限仕様

core のレート制限はすべてプロセス内メモリのトークンバケット (governor クレート) で実装されている。実装根拠は `core/src/auth/rate_limit.rs`、`core/src/state.rs`、`core/src/auth/extractor.rs`、`core/src/public.rs`。

## 系統

| 系統 | 対象 | 制限キー | 既定値 | 適用箇所 |
| --- | --- | --- | --- | --- |
| `PublicRateLimiter` | `/public/{key}` | クライアント IP | 300 req/min (`BOOSKIFF_PUBLIC_RATE_LIMIT_RPM`) | `public.rs` (DB 参照前) |
| `RateLimiters` (owner 用) | 認証済み API 全般 | owner キー | プラン由来 (`plan_default` 100 / `plan_premium` 300 req/min、課金ルールで上書き可) | `auth/extractor.rs` |
| `RateLimiters` (admin 用) | 管理 API | 管理者トークン由来キー | `plan_default_rate_limit_rpm` | `auth/admin_auth.rs` |

- 超過時の応答は 429、エラーコード `rate_limited`。
- rpm 0 は 1 にクランプされる (governor が非ゼロ quota を要求するため)。

## プロセス内 state (マルチレプリカ caveat)

- どのリミッタもカウンタをプロセス内メモリに保持し、レプリカ間で共有しない。`PublicRateLimiter` のコードコメントにも "Process-local public budgets; replicas do not share their counters" と明記されている。
- したがって水平スケール時の実効制限は最大でレプリカ数倍に発散する。例: 既定 300 rpm の設定で 3 レプリカにロードバランスすると、同一クライアントは合計で最大 900 req/min まで通り得る (各プロセスが独立に 300 を許可する)。
- プロセス再起動で全カウンタがリセットされ、burst が復活する。
- 厳密なグローバル制限が必要な場合は CDN / ロードバランサ側のレート制限を併用し、core の制限は per-プロセスの防御線として扱うこと。

## その他の実装上の特性

- 課金ルール編集等で owner の rpm が変わると、そのキーのリミッタは新 quota で再生成される。消費済み分はリセットされ burst が復活する (`state.rs`)。既存バケットの残量と新 quota の調停コストを避ける意図的な設計。
- `RateLimiters` は追跡オーナーが 50,000 に達した状態で未追跡キーが来ると map 全体をクリアする (`MAX_TRACKED_OWNERS`)。リミッタは安価に再充填されるため、クリアされたオーナーは小さな burst 許容を取り戻すだけ、という割り切り。
- `PublicRateLimiter` は 10,000 チェックごとに `retain_recent` を呼び、補充済みエントリを掃除する。distinct IP の大量流入でメモリが無限に増えないようにするための周期的な回収であり、厳密なサイズ上限ではない。
- `BOOSKIFF_TRUST_PROXY_HEADERS` による制限キー (IP) の決定規則と fail-open 挙動は [public-access.md](public-access.md) を参照。
- 課金解決キャッシュ (`BillingCache`) も同じくプロセス内メモリであり、レプリカ間で鮮度が発散し得る点は [../operations.md](../operations.md) を参照。
