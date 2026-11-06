mod batch;
mod experiments;
mod geo;
mod highlight;
mod hybrid;
mod personalization;
mod pipeline;
pub mod request;
mod reranking;
mod response;
mod single;
mod single_execution;
mod single_interleaving;
mod single_support;
mod synonyms;
mod transforms;

// Re-export public API
pub use batch::batch_search;
pub(crate) use request::build_params_echo;
pub use request::resolve_search_mode;
pub use single::{search, search_get, search_single};

// Re-export utoipa-generated path structs for openapi.rs
pub use batch::__path_batch_search;
pub use single::__path_search;
pub use single::__path_search_get;

// Private re-imports so that `super::*` from the #[cfg(test)] block below
// continues to resolve after functions moved to sub-modules.
#[cfg(test)]
use experiments::*;
#[cfg(test)]
use geo::*;
#[cfg(test)]
use personalization::*;
#[cfg(test)]
use pipeline::*;
#[cfg(test)]
use request::*;
#[cfg(test)]
use reranking::*;
// Types needed by test code that were previously imported at module scope.
#[cfg(test)]
use super::AppState;
#[cfg(test)]
use crate::dto::SearchRequest;
#[cfg(test)]
use flapjack::types::ScoredDocument;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "stage5_integration_tests.rs"]
mod stage5_integration_tests;

/// Capture once at admission so fallback and grouped work retain their view.
pub(super) fn capture_required_configuration(
    state: &crate::handlers::AppState,
    application_id: Option<&str>,
    required: Option<crate::auth::RequiredApplicationConfiguration>,
) -> Result<
    Option<std::sync::Arc<flapjack::dictionaries::manager::DictionarySnapshot>>,
    flapjack::error::FlapjackError,
> {
    let Some(application) = application_id else {
        return if required.is_some() {
            Err(
                flapjack::error::FlapjackError::ApplicationConfigurationUnavailable(
                    "required application identity missing".into(),
                ),
            )
        } else {
            Ok(None)
        };
    };
    let snapshot = match required {
        Some(required) => state
            .dictionary_manager
            .snapshot_at_least(application, required.0),
        None => state.dictionary_manager.snapshot(application),
    };
    snapshot
        .map(|snapshot| Some(std::sync::Arc::new(snapshot)))
        .map_err(|error| {
            flapjack::error::FlapjackError::ApplicationConfigurationUnavailable(error.to_string())
        })
}
