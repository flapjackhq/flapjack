#[allow(dead_code)] // Shared across integration-test binaries; each target uses only a subset.
pub mod assertions;
#[allow(dead_code)] // Shared across integration-test binaries; each target uses only a subset.
pub mod env;
#[allow(dead_code)] // Shared across integration-test binaries; each target uses only a subset.
pub mod fixtures;
#[allow(dead_code)] // Shared across integration-test binaries; each target uses only a subset.
pub mod http;
#[allow(dead_code)] // Shared across integration-test binaries; each target uses only a subset.
pub mod search_compat;
#[allow(dead_code)] // Shared across integration-test binaries; each target uses only a subset.
pub mod state;

#[allow(unused_imports)] // Re-export facade for tests that import helpers from `common::*`.
pub use assertions::*;
#[allow(unused_imports)] // Re-export facade for tests that import helpers from `common::*`.
pub use env::*;
#[allow(unused_imports)] // Re-export facade for tests that import helpers from `common::*`.
pub use fixtures::*;
#[allow(unused_imports)] // Re-export facade for tests that import helpers from `common::*`.
pub use http::*;
#[allow(unused_imports)] // Re-export facade for tests that import helpers from `common::*`.
pub use search_compat::*;
#[allow(unused_imports)] // Re-export facade for tests that import helpers from `common::*`.
pub use state::*;
