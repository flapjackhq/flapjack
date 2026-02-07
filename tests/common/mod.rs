use axum::{
    middleware,
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::TcpListener;

#[allow(dead_code)]
pub async fn spawn_server() -> (String, TempDir) {
    spawn_server_with_key(None).await
}

pub async fn spawn_server_with_key(admin_key: Option<&str>) -> (String, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let manager = flapjack::IndexManager::new(temp_dir.path());

    let key_store = admin_key.map(|k| {
        Arc::new(flapjack_http::auth::KeyStore::load_or_create(
            temp_dir.path(),
            k,
        ))
    });

    let state = Arc::new(flapjack_http::handlers::AppState {
        manager,
        key_store: key_store.clone(),
        replication_manager: None,
        ssl_manager: None,
    });

    let key_routes = if let Some(ref ks) = key_store {
        Router::new()
            .route(
                "/1/keys",
                post(flapjack_http::handlers::create_key).get(flapjack_http::handlers::list_keys),
            )
            .route(
                "/1/keys/:key",
                get(flapjack_http::handlers::get_key)
                    .put(flapjack_http::handlers::update_key)
                    .delete(flapjack_http::handlers::delete_key),
            )
            .route(
                "/1/keys/:key/restore",
                post(flapjack_http::handlers::restore_key),
            )
            .with_state(ks.clone())
    } else {
        Router::new()
    };

    let health_route = Router::new()
        .route("/health", get(flapjack_http::handlers::health))
        .with_state(state.clone());

    let protected = Router::new()
        .route("/1/indexes", post(flapjack_http::handlers::create_index))
        .route("/1/indexes", get(flapjack_http::handlers::list_indices))
        .route(
            "/1/indexes/:indexName/batch",
            post(flapjack_http::handlers::add_documents),
        )
        .route(
            "/1/indexes/:indexName/query",
            post(flapjack_http::handlers::search),
        )
        .route(
            "/1/indexes/:indexName/queries",
            post(flapjack_http::handlers::batch_search),
        )
        .route(
            "/1/indexes/:indexName/settings",
            post(flapjack_http::handlers::set_settings),
        )
        .route(
            "/1/indexes/:indexName/settings",
            get(flapjack_http::handlers::get_settings),
        )
        .route(
            "/1/indexes/:indexName",
            delete(flapjack_http::handlers::delete_index),
        )
        .route("/1/tasks/:task_id", get(flapjack_http::handlers::get_task))
        .with_state(state);

    let ks_for_middleware = key_store.clone();
    let auth_middleware = middleware::from_fn(
        move |mut request: axum::extract::Request, next: middleware::Next| {
            let ks = ks_for_middleware.clone();
            async move {
                if let Some(ref store) = ks {
                    request.extensions_mut().insert(store.clone());
                }
                flapjack_http::auth::authenticate_and_authorize(request, next).await
            }
        },
    );

    let app = Router::new()
        .merge(health_route)
        .merge(key_routes)
        .merge(protected)
        .layer(auth_middleware);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    (addr, temp_dir)
}
