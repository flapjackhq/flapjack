//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/may31_12pm_4_idempotency_cache_durability/flapjack_dev/engine/flapjack-http/src/router.rs.
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    middleware,
    routing::{delete, get, post},
    Router,
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::auth::{
    authenticate_and_authorize, request_application_id, AuthenticatedAppId, KeyStore, RateLimiter,
};
use crate::handlers;
use crate::handlers::analytics;
use crate::handlers::insights::GdprDeleteState;
use crate::handlers::{
    add_documents, add_record_auto_id, append_security_source, batch_search, browse_index,
    chat_index, clear_index, clear_rules, clear_synonyms, compact_index, create_index,
    delete_by_query, delete_index, delete_object, delete_rule, delete_security_source,
    delete_synonym, get_object, get_objects, get_rule, get_security_sources, get_synonym, get_task,
    get_task_for_index, health, list_algolia_indexes, list_indices, migrate_from_algolia,
    operation_index, partial_update_object, put_object, query_suggestions, ready,
    replace_security_sources, save_rule, save_rules, save_synonym, save_synonyms, search,
    search_facet_values, search_get, search_rules, search_synonyms, AppState,
};
use crate::handlers::{dashboard::dashboard_handler, internal, metrics};
use crate::latency_middleware::observe_request_latency;
use crate::middleware::{
    allow_private_network, ensure_json_errors, normalize_content_type, request_id_middleware,
    TrustedProxyMatcher,
};
use crate::openapi::ApiDoc;
use crate::security_sources::SecuritySourcesMatcher;
use crate::startup::CorsMode;
use flapjack::analytics::AnalyticsCollector;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// Constructs the full Axum router: mounts all route groups (index CRUD, search,
/// keys, analytics, experiments, internal ops), applies middleware, and attaches
/// the dashboard and OpenAPI/Swagger UI.
pub fn build_router(
    state: Arc<AppState>,
    key_store: Option<Arc<KeyStore>>,
    analytics_collector: Arc<AnalyticsCollector>,
    trusted_proxy_matcher: Arc<TrustedProxyMatcher>,
    cors_mode: CorsMode,
    data_dir: &Path,
) -> Router {
    let auth_enabled = key_store.is_some();
    let app = Router::new()
        .merge(build_health_routes(state.clone()))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .merge(build_key_routes(key_store.clone()))
        .merge(build_protected_routes(state.clone(), data_dir))
        .merge(build_analytics_routes(
            state.clone(),
            Arc::clone(&analytics_collector),
        ))
        .merge(build_experiments_routes(state.clone()))
        .merge(build_insights_routes(analytics_collector, data_dir))
        .merge(build_internal_routes(state.clone(), auth_enabled));

    let app = app
        .route("/dashboard/", get(dashboard_handler))
        .nest("/dashboard", Router::new().fallback(get(dashboard_handler)));

    apply_middleware(app, state, trusted_proxy_matcher, key_store, &cors_mode)
}

fn build_health_routes(state: Arc<AppState>) -> Router {
    build_public_health_routes()
        .route("/metrics", get(metrics::metrics_handler))
        .with_state(state)
}

/// Build the shared public health routes that must remain auth-free in both
/// production and integration-test helper routers.
pub fn build_public_health_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(ready))
}

/// Builds API key management routes (CRUD for keys), skipped if auth is disabled.
fn build_key_routes(key_store: Option<Arc<KeyStore>>) -> Router {
    if let Some(store) = key_store {
        Router::new()
            .route(
                "/1/keys",
                post(handlers::create_key).get(handlers::list_keys),
            )
            .route(
                "/1/keys/:key",
                get(handlers::get_key)
                    .put(handlers::update_key)
                    .delete(handlers::delete_key),
            )
            .route("/1/keys/:key/restore", post(handlers::restore_key))
            .route(
                "/1/keys/generateSecuredApiKey",
                post(handlers::generate_secured_key),
            )
            .with_state(store)
    } else {
        Router::new()
    }
}

/// Builds all auth-protected routes: indexing, search, objects, settings, synonyms, rules.
fn build_protected_routes(state: Arc<AppState>, data_dir: &Path) -> Router {
    let protected = Router::new()
        .route("/1/indexes", post(create_index).get(list_indices))
        .route("/1/indexes/:indexName/browse", post(browse_index))
        .route("/1/indexes/:indexName/chat", post(chat_index))
        .route("/1/indexes/:indexName/clear", post(clear_index))
        .route("/1/indexes/:indexName/compact", post(compact_index))
        .route("/1/indexes/:indexName/batch", post(add_documents))
        .route("/1/indexes/:indexName/query", post(search).get(search_get))
        .route("/1/indexes/:indexName/deleteByQuery", post(delete_by_query))
        .route(
            "/1/indexes/:indexName/facets/:facetName/query",
            post(search_facet_values),
        )
        .route(
            "/1/indexes/:indexName/facets/:facetName/searchForFacetValues",
            post(search_facet_values),
        )
        .route("/1/indexes/:indexName/synonyms/:objectID", get(get_synonym))
        .route(
            "/1/indexes/:indexName/synonyms/:objectID",
            axum::routing::put(save_synonym),
        )
        .route(
            "/1/indexes/:indexName/synonyms/:objectID",
            delete(delete_synonym),
        )
        .route("/1/indexes/:indexName/synonyms/batch", post(save_synonyms))
        .route("/1/indexes/:indexName/synonyms/clear", post(clear_synonyms))
        .route(
            "/1/indexes/:indexName/synonyms/search",
            post(search_synonyms),
        )
        .route("/1/indexes/:indexName/rules/:objectID", get(get_rule))
        .route(
            "/1/indexes/:indexName/rules/:objectID",
            axum::routing::put(save_rule),
        )
        .route("/1/indexes/:indexName/rules/:objectID", delete(delete_rule))
        .route("/1/indexes/:indexName/rules/batch", post(save_rules))
        .route("/1/indexes/:indexName/rules/clear", post(clear_rules))
        .route("/1/indexes/:indexName/rules/search", post(search_rules))
        .route("/1/indexes/:indexName/operation", post(operation_index))
        .route(
            "/1/indexes/:indexName/export",
            get(handlers::snapshot::export_snapshot),
        )
        .route(
            "/1/indexes/:indexName/import",
            post(handlers::snapshot::import_snapshot),
        )
        .route(
            "/1/indexes/:indexName/snapshot",
            post(handlers::snapshot::snapshot_to_s3),
        )
        .route(
            "/1/indexes/:indexName/restore",
            post(handlers::snapshot::restore_from_s3),
        )
        .route(
            "/1/indexes/:indexName/snapshots",
            get(handlers::snapshot::list_s3_snapshots),
        )
        .route("/1/indexes/:indexName/queries", post(batch_search))
        .route("/1/indexes/:indexName/objects", post(get_objects))
        .route(
            "/1/indexes/:indexName/settings",
            get(handlers::get_settings)
                .post(handlers::set_settings)
                .put(handlers::set_settings),
        )
        .route(
            "/1/indexes/:indexName/:model/recommend/rules/batch",
            post(handlers::recommend_rules::batch_recommend_rules),
        )
        .route(
            "/1/indexes/:indexName/:model/recommend/rules/search",
            post(handlers::recommend_rules::search_recommend_rules),
        )
        .route(
            "/1/indexes/:indexName/:model/recommend/rules/:objectID",
            get(handlers::recommend_rules::get_recommend_rule)
                .put(handlers::recommend_rules::put_recommend_rule)
                .delete(handlers::recommend_rules::delete_recommend_rule),
        )
        .route(
            "/1/indexes/:indexName/:objectID/partial",
            post(partial_update_object).put(partial_update_object),
        )
        .route("/1/indexes/:indexName/:objectID", get(get_object))
        .route("/1/indexes/:indexName/:objectID", delete(delete_object))
        .route(
            "/1/indexes/:indexName/:objectID",
            axum::routing::put(put_object),
        )
        .route(
            "/1/indexes/:indexName",
            get(search_get)
                .post(add_record_auto_id)
                .delete(delete_index),
        )
        .route("/1/migrate-from-algolia", post(migrate_from_algolia))
        .route("/1/usage/:statistic", get(handlers::usage::usage_global))
        .route(
            "/1/usage/:statistic/:indexName",
            get(handlers::usage::usage_per_index),
        )
        .route("/1/algolia-list-indexes", post(list_algolia_indexes))
        .route("/1/task/:task_id", get(get_task))
        .route("/1/tasks/:task_id", get(get_task))
        .route(
            "/1/indexes/:indexName/task/:task_id",
            get(get_task_for_index),
        )
        .route(
            "/1/configs",
            get(query_suggestions::list_configs).post(query_suggestions::create_config),
        )
        .route(
            "/1/configs/:indexName",
            get(query_suggestions::get_config)
                .put(query_suggestions::update_config)
                .delete(query_suggestions::delete_config),
        )
        .route(
            "/1/configs/:indexName/status",
            get(query_suggestions::get_status),
        )
        .route(
            "/1/configs/:indexName/build",
            post(query_suggestions::trigger_build),
        )
        .route("/1/logs/:indexName", get(query_suggestions::get_logs))
        .route(
            "/1/dictionaries/:dictionaryName/batch",
            post(handlers::dictionaries::dictionary_batch),
        )
        .route(
            "/1/dictionaries/:dictionaryName/search",
            post(handlers::dictionaries::dictionary_search),
        )
        .route(
            "/1/dictionaries/:_wildcard/settings",
            get(handlers::dictionaries::dictionary_get_settings)
                .put(handlers::dictionaries::dictionary_set_settings),
        )
        .route(
            "/1/dictionaries/:_wildcard/languages",
            get(handlers::dictionaries::dictionary_list_languages),
        )
        .route(
            "/1/security/sources",
            get(get_security_sources).put(replace_security_sources),
        )
        .route("/1/security/sources/append", post(append_security_source))
        .route(
            "/1/security/sources/:source",
            delete(delete_security_source),
        )
        .route(
            "/1/strategies/personalization",
            post(handlers::personalization::set_personalization_strategy)
                .get(handlers::personalization::get_personalization_strategy)
                .delete(handlers::personalization::delete_personalization_strategy),
        )
        .route(
            "/1/profiles/personalization/:userToken",
            get(handlers::personalization::get_user_profile),
        )
        .route(
            "/1/profiles/:userToken",
            delete(handlers::personalization::delete_user_profile),
        )
        .route(
            "/1/indexes/:_wildcard/recommendations",
            post(handlers::recommend::recommend),
        )
        .with_state(state.clone());

    let security_sources_matcher = Arc::new(SecuritySourcesMatcher::new(data_dir));
    let protected = protected.layer(middleware::from_fn(
        move |request: axum::extract::Request, next: middleware::Next| {
            let matcher = security_sources_matcher.clone();
            async move {
                crate::security_sources::enforce_security_sources(request, next, &matcher).await
            }
        },
    ));

    protected.layer(middleware::from_fn(
        move |request: axum::extract::Request, next: middleware::Next| {
            let counters = state.usage_counters.clone();
            async move {
                crate::usage_middleware::usage_counting_layer(request, next, &counters).await
            }
        },
    ))
}

fn build_internal_routes(state: Arc<AppState>, auth_enabled: bool) -> Router {
    let public_routes = Router::new()
        .route(
            "/.well-known/acme-challenge/:token",
            get(internal::acme_challenge),
        )
        .with_state(state.clone());

    // Peer-health routes are always available (needed for HA probing even in no-auth mode)
    let peer_health_routes = Router::new()
        .route("/internal/status", get(internal::replication_status))
        .route("/internal/cluster/status", get(internal::cluster_status))
        .with_state(state.clone());

    // Replication/catch-up routes must stay reachable in no-auth HA deployments
    // (compose sets FLAPJACK_NO_AUTH=1). Without these endpoints, peers get 404s
    // and anti-entropy cannot deliver writes.
    let replication_mesh_routes = Router::new()
        .route("/internal/replicate", post(internal::replicate_ops))
        .route("/internal/ops", get(internal::get_ops))
        .route("/internal/tenants", get(internal::list_tenants))
        .route(
            "/internal/snapshot/:tenantId",
            get(internal::internal_snapshot),
        )
        .route(
            "/internal/analytics-rollup",
            post(internal::receive_analytics_rollup),
        )
        .with_state(state.clone());

    let internal_routes = if auth_enabled {
        // Auth mode: expose all internal routes plus admin key rotation.
        let admin_internal_routes = Router::new()
            .route("/internal/rollup-cache", get(internal::rollup_cache_status))
            .route("/internal/storage", get(internal::storage_all))
            .route("/internal/storage/:indexName", get(internal::storage_index))
            .route("/internal/pause/:indexName", post(internal::pause_index))
            .route("/internal/resume/:indexName", post(internal::resume_index))
            .route(
                "/internal/rotate-admin-key",
                post(internal::rotate_admin_key),
            )
            .with_state(state.clone());
        Router::new()
            .merge(peer_health_routes)
            .merge(replication_mesh_routes)
            .merge(admin_internal_routes)
    } else {
        // No-auth mode still needs replication/catch-up routes for HA convergence.
        Router::new()
            .merge(peer_health_routes)
            .merge(replication_mesh_routes)
    };

    public_routes.merge(internal_routes)
}

/// Builds analytics API routes for top searches, click-through, and conversion rates.
fn build_analytics_routes(state: Arc<AppState>, _collector: Arc<AnalyticsCollector>) -> Router {
    let analytics_engine = state
        .analytics_engine
        .as_ref()
        .expect("Analytics engine should be initialized");

    let analytics_routes = Router::new()
        .route("/2/searches", get(analytics::get_top_searches))
        .route("/2/searches/count", get(analytics::get_search_count))
        .route("/2/searches/noResults", get(analytics::get_no_results))
        .route(
            "/2/searches/noResultRate",
            get(analytics::get_no_result_rate),
        )
        .route("/2/searches/noClicks", get(analytics::get_no_clicks))
        .route("/2/searches/noClickRate", get(analytics::get_no_click_rate))
        .route(
            "/2/clicks/clickThroughRate",
            get(analytics::get_click_through_rate),
        )
        .route(
            "/2/clicks/averageClickPosition",
            get(analytics::get_average_click_position),
        )
        .route("/2/clicks/positions", get(analytics::get_click_positions))
        .route(
            "/2/conversions/conversionRate",
            get(analytics::get_conversion_rate),
        )
        .route(
            "/2/conversions/addToCartRate",
            get(analytics::get_add_to_cart_rate),
        )
        .route(
            "/2/conversions/purchaseRate",
            get(analytics::get_purchase_rate),
        )
        .route("/2/conversions/revenue", get(analytics::get_revenue))
        .route("/2/hits", get(analytics::get_top_hits))
        .route("/2/filters", get(analytics::get_top_filters))
        .route(
            "/2/filters/noResults",
            get(analytics::get_filters_no_results),
        )
        .route("/2/filters/:attribute", get(analytics::get_filter_values))
        .route("/2/users/count", get(analytics::get_users_count))
        .route("/2/status", get(analytics::get_analytics_status))
        .route("/2/devices", get(analytics::get_device_breakdown))
        .route("/2/countries", get(analytics::get_countries))
        .route("/2/geo", get(analytics::get_geo_breakdown))
        .route("/2/geo/:country", get(analytics::get_geo_top_searches))
        .route("/2/geo/:country/regions", get(analytics::get_geo_regions))
        .route("/2/overview", get(analytics::get_overview))
        .route("/2/analytics/seed", post(analytics::seed_analytics))
        .route("/2/analytics/clear", delete(analytics::clear_analytics))
        .route("/2/analytics/flush", post(analytics::flush_analytics));

    let analytics_routes = analytics_routes.with_state(Arc::clone(analytics_engine));

    let analytics_cleanup_routes = Router::new()
        .route("/2/analytics/cleanup", post(analytics::cleanup_analytics))
        .with_state(state);

    analytics_routes.merge(analytics_cleanup_routes)
}

/// Builds A/B testing experiment management routes.
fn build_experiments_routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/2/abtests",
            post(handlers::experiments::create_experiment)
                .get(handlers::experiments::list_experiments),
        )
        .route(
            "/2/abtests/estimate",
            post(handlers::experiments::estimate_ab_test),
        )
        .route(
            "/2/abtests/:id",
            get(handlers::experiments::get_experiment)
                .put(handlers::experiments::update_experiment)
                .delete(handlers::experiments::delete_experiment),
        )
        .route(
            "/2/abtests/:id/start",
            post(handlers::experiments::start_experiment),
        )
        .route(
            "/2/abtests/:id/stop",
            post(handlers::experiments::stop_experiment),
        )
        .route(
            "/2/abtests/:id/conclude",
            post(handlers::experiments::conclude_experiment),
        )
        .route(
            "/2/abtests/:id/results",
            get(handlers::experiments::get_experiment_results),
        )
        .with_state(state)
}

/// Builds click/conversion event ingestion and GDPR profile-deletion routes.
fn build_insights_routes(analytics_collector: Arc<AnalyticsCollector>, data_dir: &Path) -> Router {
    let gdpr_delete_state = GdprDeleteState {
        analytics_collector: analytics_collector.clone(),
        profile_store_base_path: data_dir.to_path_buf(),
    };

    Router::new()
        .route("/1/events", post(handlers::insights::post_events))
        .route("/1/events/debug", get(handlers::insights::get_debug_events))
        .with_state(analytics_collector)
        .merge(
            Router::new()
                .route(
                    "/1/usertokens/:userToken",
                    delete(handlers::insights::delete_usertoken),
                )
                .with_state(gdpr_delete_state),
        )
}

pub(crate) fn build_cors_layer(mode: &CorsMode) -> CorsLayer {
    let max_age = std::time::Duration::from_secs(86400);
    match mode {
        CorsMode::LoopbackOnly => CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(is_loopback_origin))
            .allow_methods(Any)
            .allow_headers(Any)
            .max_age(max_age),
        CorsMode::Restricted(origins) => CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins.iter().cloned()))
            .allow_methods(Any)
            .allow_headers(Any)
            .max_age(max_age),
    }
}

fn is_loopback_origin(origin: &axum::http::HeaderValue, _: &axum::http::request::Parts) -> bool {
    let Ok(raw_origin) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = raw_origin.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let Some(host) = uri.host() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let normalized_host = host.trim_start_matches('[').trim_end_matches(']');
    normalized_host
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Parse `FLAPJACK_MAX_BODY_MB`-style values.
///
/// Missing, empty, or invalid values use the default of 100 MB.
/// Zero is a valid parse result (matches `usize` semantics).
pub(crate) fn max_body_mb_from_value(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.parse().ok()).unwrap_or(100)
}

pub(crate) fn app_id_layer(app: Router) -> Router {
    app.layer(middleware::from_fn(
        |mut request: axum::extract::Request, next: middleware::Next| async move {
            if request.extensions().get::<AuthenticatedAppId>().is_none() {
                // Dictionaries still need a tenant key in open mode, where auth is intentionally skipped.
                let application_id = request_application_id(&request).unwrap_or_else(|| {
                    flapjack::dictionaries::DEFAULT_DICTIONARY_TENANT.to_string()
                });
                request
                    .extensions_mut()
                    .insert(AuthenticatedAppId(application_id));
            }
            next.run(request).await
        },
    ))
}

fn apply_middleware(
    app: Router,
    state: Arc<AppState>,
    trusted_proxy_matcher: Arc<TrustedProxyMatcher>,
    key_store: Option<Arc<KeyStore>>,
    cors_mode: &CorsMode,
) -> Router {
    let max_body_mb: usize =
        max_body_mb_from_value(std::env::var("FLAPJACK_MAX_BODY_MB").ok().as_deref());

    let mgr_for_pressure = Arc::clone(&state.manager);
    let default_facet_cache_cap = state
        .manager
        .facet_cache_cap
        .load(std::sync::atomic::Ordering::Relaxed);

    let memory_middleware = middleware::from_fn(
        move |request: axum::extract::Request, next: middleware::Next| {
            let manager = mgr_for_pressure.clone();
            async move {
                crate::memory_middleware::memory_pressure_guard(
                    request,
                    next,
                    &manager,
                    default_facet_cache_cap,
                )
                .await
            }
        },
    );

    // Auth layer — only applied when authentication is enabled (KeyStore present).
    // In open mode (--no-auth), the layer is omitted entirely so requests never
    // enter authenticate_and_authorize and never depend on RateLimiter/TrustedProxyMatcher.
    let app = if let Some(ks) = key_store {
        let rate_limiter = RateLimiter::new();
        let trusted_proxies = trusted_proxy_matcher.clone();
        let auth_layer = middleware::from_fn(
            move |mut request: axum::extract::Request, next: middleware::Next| {
                let ks = ks.clone();
                let rl = rate_limiter.clone();
                let tp = trusted_proxies.clone();
                async move {
                    request.extensions_mut().insert(ks);
                    request.extensions_mut().insert(tp);
                    request.extensions_mut().insert(rl);
                    authenticate_and_authorize(request, next).await
                }
            },
        );
        app.layer(auth_layer)
    } else {
        app
    };

    app_id_layer(app)
        .layer(memory_middleware)
        .layer(DefaultBodyLimit::max(max_body_mb * 1024 * 1024))
        .layer(middleware::from_fn(normalize_content_type))
        .layer(middleware::from_fn(ensure_json_errors))
        .layer(build_cors_layer(cors_mode))
        .layer(middleware::from_fn(allow_private_network))
        .layer(middleware::from_fn(observe_request_latency))
        .layer(middleware::from_fn(request_id_middleware))
}

#[cfg(test)]
#[path = "router_inline_tests.rs"]
mod tests;
