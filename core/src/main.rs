use axum::Json;
use axum::response::Redirect;
use axum::routing::get;
use core::api_doc::openapi_spec;
use core::auth::admin_auth::{find_active_by_hash, hash_admin_token};
use core::auth::jwks::JwksCache;
use core::config::Config;
use core::error::AppError;
use core::state::{AppState, RateLimiters};
use core::{admin, billing, drive, health, public, storage::Storage};
use std::sync::Arc;
use std::time::Duration;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

fn main() {
    // The process-wide aws-lc-rs provider must be installed before any
    // rustls consumer initializes (sqlx TLS, reqwest, presigning).
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    if std::env::args().nth(1).as_deref() == Some("openapi") {
        match openapi_spec().to_json() {
            Ok(spec) => print!("{spec}"),
            Err(err) => {
                eprintln!("failed to serialize openapi spec: {err}");
                std::process::exit(1);
            }
        }
        return;
    }

    // Manual multi-thread runtime instead of `#[tokio::main]`: the macro's
    // expansion references `core::` paths, which this package's own lib
    // name (`core`) shadows.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| {
            eprintln!("failed to start tokio runtime: {err}");
            std::process::exit(1);
        });
    runtime.block_on(run());
}

async fn run() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = match Config::load() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("configuration error: {err}");
            std::process::exit(1);
        }
    };
    let listen_addr = config.listen_addr.clone();

    let pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
    {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("failed to connect to database: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = sqlx::migrate!("./migrations").run(&pool).await {
        eprintln!("failed to run database migrations: {err}");
        std::process::exit(1);
    }
    if let Some(token) = &config.admin_bootstrap_token
        && let Err(err) = seed_bootstrap_admin_token(&pool, token).await
    {
        eprintln!("failed to seed bootstrap admin token: {err}");
        std::process::exit(1);
    }

    let storage = match Storage::build(&config).await {
        Ok(storage) => storage,
        Err(err) => {
            eprintln!("failed to initialize object storage: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = storage.ensure_bucket().await {
        eprintln!("failed to prepare object storage bucket: {err}");
        std::process::exit(1);
    }

    let jwks_cache = JwksCache::new(config.jwt_trusted_issuers.clone());
    let upload_routes = drive::files::upload_router(&config);
    let files_routes = drive::files::files_router();
    let state = AppState {
        pool,
        s3: storage,
        config,
        jwks_cache,
        rate_limiters: Arc::new(RateLimiters::default()),
    };

    // Stateful routers merge first; state is baked in once at the end
    // (axum 0.8 lacks a Router<AppState> -> Router<()> late conversion).
    // `upload_routes` is merged one level up so the 120 s request timeout
    // never kills a large, slow upload; everything else stays capped.
    let app = axum::Router::<AppState>::new()
        .merge(upload_routes)
        .merge(
            axum::Router::<AppState>::new()
                .merge(billing::status_handler::billing_status_router())
                .merge(files_routes)
                .nest("/v1/folders", drive::folders::folders_router())
                .nest("/v1/admin", admin::admin_router())
                .merge(public::public_router())
                .merge(health::health_router())
                .route("/openapi.json", get(|| async { Json(openapi_spec()) }))
                .route("/docs", get(docs_redirect))
                .merge(
                    utoipa_swagger_ui::SwaggerUi::new("/swagger-ui")
                        .url("/api-docs/openapi.json", openapi_spec()),
                )
                .layer(TimeoutLayer::with_status_code(
                    axum::http::StatusCode::REQUEST_TIMEOUT,
                    Duration::from_secs(120),
                )),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .unwrap_or_else(|err| {
            eprintln!("failed to bind {listen_addr}: {err}");
            std::process::exit(1);
        });
    tracing::info!("listening on http://{listen_addr}");
    if let Err(err) = axum::serve(listener, app).await {
        eprintln!("server error: {err}");
        std::process::exit(1);
    }
}

/// Seed `config.admin_bootstrap_token` as an active `admin_tokens` row
/// (name `bootstrap`) unless an active token with that hash already
/// exists, making restarts with the same environment idempotent.
async fn seed_bootstrap_admin_token(pool: &sqlx::PgPool, token: &str) -> Result<(), AppError> {
    let token_hash = hash_admin_token(token);
    if find_active_by_hash(pool, &token_hash).await?.is_some() {
        tracing::info!("bootstrap admin token already seeded");
        return Ok(());
    }
    sqlx::query("INSERT INTO admin_tokens (name, token_hash) VALUES ('bootstrap', $1) ON CONFLICT (name) DO UPDATE SET token_hash = EXCLUDED.token_hash, revoked_at = NULL")
        .bind(&token_hash)
        .execute(pool)
        .await
        .map_err(|err| AppError::Internal(format!("seed bootstrap admin token: {err}")))?;
    tracing::info!("seeded bootstrap admin token");
    Ok(())
}

async fn docs_redirect() -> Redirect {
    Redirect::to("/swagger-ui/")
}
