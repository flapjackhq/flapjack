use crate::auth::KeyStore;
use flapjack::IndexManager;
use flapjack::SslManager;
use flapjack_replication::manager::ReplicationManager;
use std::sync::Arc;

pub mod browse;
pub mod facets;
pub mod health;
pub mod indices;
pub mod internal;
pub mod keys;
pub mod migration;
pub mod objects;
pub mod rules;
pub mod search;
pub mod settings;
pub mod snapshot;
pub mod synonyms;
pub mod tasks;

pub struct AppState {
    pub manager: Arc<IndexManager>,
    pub key_store: Option<Arc<KeyStore>>,
    pub replication_manager: Option<Arc<ReplicationManager>>,
    pub ssl_manager: Option<Arc<SslManager>>,
}

pub use browse::browse_index;
pub use facets::{parse_facet_params, search_facet_values};
pub use health::health;
pub use indices::{clear_index, create_index, delete_index, list_indices, operation_index};
pub use keys::{
    create_key, delete_key, generate_secured_key, get_key, list_keys, restore_key, update_key,
};
pub use migration::migrate_from_algolia;
pub use objects::{
    add_documents, delete_by_query, delete_object, get_object, get_objects, put_object,
};
pub use rules::{clear_rules, delete_rule, get_rule, save_rule, save_rules, search_rules};
pub use search::{batch_search, search};
pub use settings::{get_settings, set_settings};
pub use synonyms::{
    clear_synonyms, delete_synonym, get_synonym, save_synonym, save_synonyms, search_synonyms,
};
pub use tasks::{get_task, get_task_for_index};
