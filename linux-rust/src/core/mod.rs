//! Pure, UI-independent core logic (no I/O). Unit tested.

// The integrator wires these into the device supervisor / hub; until then parts of
// the API are only exercised by tests.
#[allow(dead_code)]
pub mod battery;
#[allow(dead_code)]
pub mod models;
