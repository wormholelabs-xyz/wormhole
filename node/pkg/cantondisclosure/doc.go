// Package cantondisclosure keeps an in-memory index of the live disclosed core
// contracts of one guardianGovernance party, with their created event blobs.
//
// The index applies one bootstrap snapshot and then one transaction at a time,
// in offset order. A create adds an entry. An archive removes the entry with
// the same contract id.
//
// A lookup returns a contract and the index offset at which the contract is
// live. Lookups fail with IndexNotReadyError when the readiness is not a ready
// state. A wrong or old answer makes the submission fail on the ledger. The
// index is therefore trusted for liveness only.
package cantondisclosure
