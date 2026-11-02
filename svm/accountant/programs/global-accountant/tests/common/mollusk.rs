use accountant_test_harness::mollusk_with_fixtures;
use mollusk_svm::Mollusk;

use super::fixtures::PROGRAM_NAME;
use super::ids::program_id;

pub fn mollusk() -> Mollusk {
    mollusk_with_fixtures(&program_id(), PROGRAM_NAME)
}
