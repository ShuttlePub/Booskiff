use axum::Json;
use axum::routing::get;
use core::api_doc::openapi_spec;
use core::auth::jwks::JwksCache;
use core::config::Config;
use core::state::{AppState, RateLimiters};
use core::{health, storage::Storage};
use std::sync::Arc;

fn main() {
    // The process-wide aws-lc-rs provider must be installed before any
    // rustls consumer initializes (sqlx TLS, reqwest, presigning).
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    if std::env::args().nth(1).as_deref() == Some("openapi") {
        match serde_json::to_string_pretty(&openapi_spec()) {
            Ok(spec) => println!("{spec}"),
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
    let state = AppState {
        pool,
        s3: storage,
        config,
        jwks_cache,
        rate_limiters: Arc::new(RateLimiters::default()),
    };

    // Stateful routers merge first; state is baked in once at the end
    // (axum 0.8 lacks a Router<AppState> -> Router<()> late conversion).
    let app = axum::Router::<AppState>::new()
        .merge(health::health_router())
        .route("/openapi.json", get(|| async { Json(openapi_spec()) }))
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/swagger-ui")
                .url("/api-docs/openapi.json", openapi_spec()),
        )
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
