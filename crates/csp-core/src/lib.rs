#![forbid(unsafe_code)]

//! Core sim primitives: integer sim time, deterministic RNG streams, canonical
//! state hashing, and the sim error taxonomy.
//!
//! Rules (PLAN §1.1, §2.3, §2.7, §2.13, §7.1):
//! - No I/O, no wall clock, no `std::time`.
//! - Sim time is an integer tick count — never an `f64` accumulator.
//! - Any `f64` math that feeds sim state uses `libm` transcendentals only.
//! - No `HashMap`/`HashSet` in anything that iterates into state or a hash.
//! - The sim never panics in release; errors surface as [`SimError`].

mod error;
mod hash;
mod rng;
mod time;
mod vec3;

pub use error::SimError;
pub use hash::{HASH_SCHEMA_VERSION, StableDiscriminant, StateHasher, bubble_mask};
pub use rng::{KDF_VERSION, StreamKey, StreamTag, derive_key, stream, uniform};
pub use time::{SimTime, TICK_HZ};
pub use vec3::DVec3;
