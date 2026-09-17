//! Test harness shared by the accountant program suites: program ids, pinned sibling
//! program fixtures, synthetic guardian sets, mollusk setup, generic VAA body and
//! instruction-data builders, result assertions, and surfpool process control.
//! Product-specific scenarios stay in each program's `tests/common`.

pub mod accounts;
pub mod fixtures;
pub mod guardians;
pub mod ids;
pub mod ix;
pub mod mollusk;
pub mod scenario;
pub mod surfpool;
pub mod wire;

pub use accounts::*;
pub use fixtures::*;
pub use guardians::*;
pub use ids::*;
pub use ix::*;
pub use mollusk::*;
pub use scenario::*;
