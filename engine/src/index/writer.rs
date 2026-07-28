use crate::index::memory::WriterGuard;
use std::ops::{Deref, DerefMut};

pub struct ManagedIndexWriter {
    inner: tantivy::IndexWriter,
    _guard: WriterGuard,
}

impl ManagedIndexWriter {
    pub(crate) fn new(inner: tantivy::IndexWriter, guard: WriterGuard) -> Self {
        ManagedIndexWriter {
            inner,
            _guard: guard,
        }
    }

    pub(crate) fn wait_merging_threads(self) -> crate::error::Result<()> {
        let ManagedIndexWriter { inner, _guard } = self;
        // Keep the budget slot until merge threads have finished writing files.
        let result = inner
            .wait_merging_threads()
            .map_err(|error| crate::error::FlapjackError::Tantivy(error.to_string()));
        drop(_guard);
        result
    }
}

impl Deref for ManagedIndexWriter {
    type Target = tantivy::IndexWriter;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for ManagedIndexWriter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
