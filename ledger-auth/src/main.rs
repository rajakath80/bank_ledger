mod auth;
mod config;
mod error;
mod handlers;
mod kanidm;

use axum::{
    Router,
    routing::{delete, get, patch, post},
};
use config::Config;
use kanidm::Kanidm;
use std::sync::Arc;
use tracing::info;

// #[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub kanidm: Kanidm,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,axum_kanidm_auth=debug".into()),
        )
        .init();

    let config = Arc::new(Config::from_env()?);
    let kanidm = Kanidm::new(&config).await?;

    // Create the application role groups if they do not exist.
    kanidm.ensure_role_groups(&config).await?;

    let state = AppState {
        config: config.clone(),
        kanidm,
    };

    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/auth/me", get(handlers::me))
        .route(
            "/admin/users",
            get(handlers::list_users).post(handlers::create_user),
        )
        .route(
            "/admin/users/{username}",
            get(handlers::get_user)
                .patch(handlers::update_user)
                .delete(handlers::delete_user),
        )
        .route("/admin/users/{username}/role", post(handlers::set_role))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    info!("listening on {}", config.bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}
