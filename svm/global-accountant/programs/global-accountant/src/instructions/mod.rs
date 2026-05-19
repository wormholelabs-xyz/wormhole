pub mod close_digest;
// `open_digest` is the public quorum-init entrypoint exposed only in test
// builds; in prod it is only reachable from `submit_observations` after the
// NoReplay check. The module body itself refuses to compile without
// `test-only-open-digest` (file-top `compile_error!` mirrors the `mock-vaa`
// pattern in `close_digest.rs`), and we gate the `mod` declaration so prod
// builds do not even need to parse the file.
#[cfg(feature = "test-only-open-digest")]
pub mod open_digest;
pub mod submit_observations;
