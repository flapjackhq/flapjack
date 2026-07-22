use flapjack::error::FlapjackError;
use flapjack::index::replica::parse_replica_entry;
use flapjack::index::rules::{Rule, RuleStore};
use flapjack::index::settings::IndexSettings;
use flapjack::index::synonyms::{Synonym, SynonymStore};
use flapjack::IndexManager;
use std::path::{Path, PathBuf};

pub(crate) trait IndexResourceStore: Sized {
    type Item;

    const FILE_NAME: &'static str;

    fn new_empty() -> Self;
    fn load(path: &Path) -> Result<Self, FlapjackError>;
    fn save(&self, path: &Path) -> Result<(), FlapjackError>;
    fn insert(&mut self, item: Self::Item);
    fn remove(&mut self, object_id: &str) -> bool;
    fn count(&self) -> usize;
    fn invalidate(manager: &IndexManager, index_name: &str);
}

impl IndexResourceStore for RuleStore {
    type Item = Rule;

    const FILE_NAME: &'static str = "rules.json";

    fn new_empty() -> Self {
        Self::new()
    }

    fn load(path: &Path) -> Result<Self, FlapjackError> {
        RuleStore::load(path)
    }

    fn save(&self, path: &Path) -> Result<(), FlapjackError> {
        RuleStore::save(self, path)
    }

    fn insert(&mut self, item: Self::Item) {
        RuleStore::insert(self, item);
    }

    fn remove(&mut self, object_id: &str) -> bool {
        RuleStore::remove(self, object_id).is_some()
    }

    fn count(&self) -> usize {
        self.all().len()
    }

    fn invalidate(manager: &IndexManager, index_name: &str) {
        manager.invalidate_rules_cache(index_name);
    }
}

impl IndexResourceStore for SynonymStore {
    type Item = Synonym;

    const FILE_NAME: &'static str = "synonyms.json";

    fn new_empty() -> Self {
        Self::new()
    }

    fn load(path: &Path) -> Result<Self, FlapjackError> {
        SynonymStore::load(path)
    }

    fn save(&self, path: &Path) -> Result<(), FlapjackError> {
        SynonymStore::save(self, path)
    }

    fn insert(&mut self, item: Self::Item) {
        SynonymStore::insert(self, item);
    }

    fn remove(&mut self, object_id: &str) -> bool {
        SynonymStore::remove(self, object_id).is_some()
    }

    fn count(&self) -> usize {
        self.values().count()
    }

    fn invalidate(manager: &IndexManager, index_name: &str) {
        manager.invalidate_synonyms_cache(index_name);
    }
}

pub(crate) fn resource_path<S: IndexResourceStore>(
    manager: &IndexManager,
    index_name: &str,
) -> PathBuf {
    manager.base_path.join(index_name).join(S::FILE_NAME)
}

pub(crate) fn load_existing_store<S: IndexResourceStore>(
    manager: &IndexManager,
    index_name: &str,
) -> Result<Option<S>, FlapjackError> {
    let path = resource_path::<S>(manager, index_name);
    if !path.exists() {
        return Ok(None);
    }

    S::load(&path).map(Some)
}

pub(crate) fn load_store_or_empty<S: IndexResourceStore>(
    manager: &IndexManager,
    index_name: &str,
) -> Result<S, FlapjackError> {
    Ok(load_existing_store::<S>(manager, index_name)?.unwrap_or_else(S::new_empty))
}

pub(crate) fn save_resource_item<S: IndexResourceStore>(
    manager: &IndexManager,
    index_name: &str,
    item: S::Item,
) -> Result<(), FlapjackError> {
    manager.create_tenant(index_name)?;

    let path = resource_path::<S>(manager, index_name);
    let mut store = load_store_or_empty::<S>(manager, index_name)?;
    store.insert(item);
    store.save(&path)?;
    S::invalidate(manager, index_name);
    Ok(())
}

pub(crate) fn save_resource_batch<S, I>(
    manager: &IndexManager,
    index_name: &str,
    items: I,
    replace_existing: bool,
) -> Result<S, FlapjackError>
where
    S: IndexResourceStore,
    I: IntoIterator<Item = S::Item>,
{
    manager.create_tenant(index_name)?;

    let path = resource_path::<S>(manager, index_name);
    let mut store = if replace_existing {
        S::new_empty()
    } else {
        load_store_or_empty::<S>(manager, index_name)?
    };

    for item in items {
        store.insert(item);
    }

    store.save(&path)?;
    S::invalidate(manager, index_name);
    Ok(store)
}

pub(crate) fn delete_resource_item<S: IndexResourceStore>(
    manager: &IndexManager,
    index_name: &str,
    object_id: &str,
) -> Result<bool, FlapjackError> {
    let path = resource_path::<S>(manager, index_name);
    let Some(mut store) = load_existing_store::<S>(manager, index_name)? else {
        return Ok(false);
    };

    if !store.remove(object_id) {
        return Ok(false);
    }

    store.save(&path)?;
    S::invalidate(manager, index_name);
    Ok(true)
}

pub(crate) fn clear_resource_store<S: IndexResourceStore>(
    manager: &IndexManager,
    index_name: &str,
) -> Result<(), FlapjackError> {
    let path = resource_path::<S>(manager, index_name);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    S::invalidate(manager, index_name);
    Ok(())
}

pub(crate) fn forward_store_to_replicas<S: IndexResourceStore>(
    manager: &IndexManager,
    primary_index_name: &str,
    store: &S,
) -> Result<(), FlapjackError> {
    let settings_path = manager
        .base_path
        .join(primary_index_name)
        .join("settings.json");
    if !settings_path.exists() {
        return Ok(());
    }

    let primary_settings = IndexSettings::load(&settings_path)?;
    let Some(replicas) = primary_settings.replicas else {
        return Ok(());
    };

    for replica_str in replicas {
        let parsed = parse_replica_entry(&replica_str)?;
        let replica_name = parsed.name();
        let replica_dir = manager.base_path.join(replica_name);
        if !replica_dir.exists() {
            continue;
        }

        let replica_path = replica_dir.join(S::FILE_NAME);
        store.save(&replica_path)?;
        S::invalidate(manager, replica_name);
    }

    Ok(())
}
