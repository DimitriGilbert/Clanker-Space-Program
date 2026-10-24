//! Minimal deterministic `f64` quaternion (PLAN §2.1: `csp-core` owns
//! "f64 math (libm only)").
//!
//! This is the shared orientation primitive the §7.2 serialized vessel
//! pose and the §2.4/§2.9 `WeldedBody`/attachment frames need — it lives
//! here for the same reason [`DVec3`] does. Hamilton convention,
//! `(x, y, z, w)` with `w` the scalar part; `q1 * q2` composes rotations
//! as "apply `q2`, then `q1`" (the usual right-to-left reading).
//!
//! The only transcendentals are `libm::{sin, cos, sqrt}` — this crate's
//! `clippy.toml` bans the `f64` intrinsic versions (§2.7/§8.2).
//!
//! **No-FMA reliance (§8.4).** The Hamilton product, [`DQuat::rotate`], and
//! the `dot`/`norm` chains are plain `a * b ± c * d` `f64` expressions.
//! rustc today lowers each to discrete `fmul`/`fadd` with no fast-math
//! flags, so LLVM cannot contract them into `fma` — but that is a codegen
//! observation, not a language guarantee. The reliance is covered by two
//! mitigations: the pinned toolchain (`rust-toolchain.toml`) and the §8.4
//! determinism fingerprint recorded beside every golden, which goes red if
//! a toolchain change ever alters the bits. The `to_bits` golden tests at
//! the bottom of this module pin current outputs on contraction-sensitive
//! inputs for the same reason.

use core::ops::Mul;

use serde::{Deserialize, Serialize};

use crate::DVec3;

/// A 4-component `f64` quaternion. Unit quaternions encode rotations;
/// this type does not enforce unit-ness — composition via
/// [`DQuat::normalize`]d products keeps drift bounded, and callers that
/// store a quaternion long-term (vessel pose) normalize on write.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DQuat {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Scalar part.
    pub w: f64,
}

impl DQuat {
    /// The identity rotation `(0, 0, 0, 1)`.
    pub const IDENTITY: Self = Self::new(0.0, 0.0, 0.0, 1.0);

    pub const fn new(x: f64, y: f64, z: f64, w: f64) -> Self {
        Self { x, y, z, w }
    }

    /// Rotation of `angle` radians about `axis`. The axis is normalized
    /// internally.
    ///
    /// Contract: `axis` must be finite and nonzero. A zero axis has no
    /// direction to rotate about, and a huge axis can overflow `norm` to
    /// `+inf` — both violate the geometry gate callers are expected to run
    /// at ingest, so they trap in debug builds (the `StateHasher` rule-4
    /// convention: invariant violations trap, never silently degrade).
    /// In release the zero case yields NaN components (which the §7.1 hash
    /// rejects as `Fatal`) and the `+inf` case yields a finite wrong
    /// quaternion — another reason the ingest gate is not optional.
    pub fn from_axis_angle(axis: DVec3, angle: f64) -> Self {
        let n = axis.norm();
        debug_assert!(
            n.is_finite() && n > 0.0,
            "DQuat::from_axis_angle: axis norm must be finite and > 0, got {n}"
        );
        let (ax, ay, az) = (axis.x / n, axis.y / n, axis.z / n);
        let half = angle * 0.5;
        let s = libm::sin(half);
        Self::new(ax * s, ay * s, az * s, libm::cos(half))
    }

    /// `q·q` — squared norm.
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }

    /// `‖q‖` — `libm::sqrt`, never `f64::sqrt` (§2.7).
    pub fn norm(self) -> f64 {
        libm::sqrt(self.dot(self))
    }

    /// `q / ‖q‖`. Normalizing every composed rotation keeps unit-quat
    /// drift bounded across long attachment chains; the op is exact
    /// IEEE + one `libm::sqrt`, so it stays bit-deterministic.
    ///
    /// Contract: `self` must be finite and nonzero — a zero quaternion has
    /// no direction to normalize. Violations trap in debug builds (the
    /// `StateHasher` rule-4 convention) and yield NaN components in
    /// release. Callers hold a `norm() > 0.0` gate at ingest (the
    /// `SetAttitude` / physical-spawn / snapshot-boundary checks in
    /// csp-sim); there is deliberately no `try_normalize` because every
    /// call site already owns that gate.
    pub fn normalize(self) -> Self {
        let n = self.norm();
        debug_assert!(
            n.is_finite() && n > 0.0,
            "DQuat::normalize: quaternion norm must be finite and > 0, got {n}"
        );
        Self::new(self.x / n, self.y / n, self.z / n, self.w / n)
    }

    /// `q*` — for a unit quaternion, the inverse rotation.
    pub fn conjugate(self) -> Self {
        Self::new(-self.x, -self.y, -self.z, self.w)
    }

    /// All four components finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.w.is_finite()
    }

    /// Rotate `v` by `self`. Exact for unit `self` up to ordinary f64
    /// rounding; no `norm` call, so it is cheap inside the weld loop.
    ///
    /// `v' = v + w·t + qᵥ × t` with `t = 2·(qᵥ × v)`.
    pub fn rotate(self, v: DVec3) -> DVec3 {
        let qv = DVec3::new(self.x, self.y, self.z);
        let t = qv.cross(v) * 2.0;
        v + t * self.w + qv.cross(t)
    }
}

impl Mul for DQuat {
    type Output = Self;
    /// Hamilton product — `a * b` is the rotation "first `b`, then `a`".
    fn mul(self, o: Self) -> Self {
        Self::new(
            self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
            self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
        )
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use core::f64::consts::{FRAC_PI_2, PI};

    #[test]
    fn identity_rotates_nothing() {
        let v = DVec3::new(1.0, -2.0, 3.5);
        assert_eq!(DQuat::IDENTITY.rotate(v), v);
    }

    #[test]
    fn quarter_turn_about_z_sends_x_to_y() {
        let q = DQuat::from_axis_angle(DVec3::Z, FRAC_PI_2);
        let v = q.rotate(DVec3::X);
        assert!((v.x).abs() < 1e-15);
        assert!((v.y - 1.0).abs() < 1e-15);
        assert!(v.z.abs() < 1e-15);
    }

    #[test]
    fn composition_applies_rightmost_first() {
        let qz = DQuat::from_axis_angle(DVec3::Z, FRAC_PI_2);
        // qz*qz = half turn about z: x -> -x.
        let v = (qz * qz).rotate(DVec3::X);
        assert!((v.x + 1.0).abs() < 1e-15);
        assert!(v.y.abs() < 1e-15);
    }

    #[test]
    fn conjugate_undoes_rotation() {
        let q = DQuat::from_axis_angle(DVec3::new(1.0, 2.0, 3.0), PI / 3.0).normalize();
        let v = DVec3::new(-2.0, 0.5, 4.0);
        let back = q.conjugate().rotate(q.rotate(v));
        assert!((back - v).norm() < 1e-15);
    }

    #[test]
    fn normalize_makes_unit() {
        let q = DQuat::new(1.0, -2.0, 0.5, 2.0).normalize();
        assert!((q.norm() - 1.0).abs() < 1e-15);
        assert!(q.is_finite());
    }

    // -- §8.4 bit-exactness canaries -------------------------------------
    // `to_bits` pins on inputs where fma contraction changes the answer
    // (verified: the fused forms differ in the last ulp on these exact
    // operands). A failure here means a toolchain/codegen change altered
    // FP semantics — re-bless only alongside a deliberate fingerprint
    // bump, never silently.

    #[test]
    fn hamilton_mul_is_bit_exact_on_contraction_sensitive_inputs() {
        let m = DQuat::new(-0.125, 0.6, 2.3, -1.75) * DQuat::new(3.0, -2.0, 0.875, 0.3);
        assert_eq!(m.x.to_bits(), 0xbfc4ccccccccccc0);
        assert_eq!(m.y.to_bits(), 0x402560f5c28f5c29);
        assert_eq!(m.z.to_bits(), 0xc0032147ae147ae1);
        assert_eq!(m.w.to_bits(), 0xbfeecccccccccccc);
    }

    #[test]
    fn rotate_is_bit_exact_on_contraction_sensitive_inputs() {
        let q = DQuat::new(1.1, 1.75, -0.1, -2.375);
        let r = q.rotate(DVec3::new(0.7, -2.7, -2.2));
        assert_eq!(r.x.to_bits(), 0x40183ae147ae147e);
        assert_eq!(r.y.to_bits(), 0xc00e79db22d0e568);
        assert_eq!(r.z.to_bits(), 0x4042a87ae147ae16);
    }

    #[test]
    fn normalize_is_bit_exact_on_contraction_sensitive_inputs() {
        let q = DQuat::new(0.4, -0.875, 0.6, 2.1).normalize();
        assert_eq!(q.x.to_bits(), 0x3fc5741c2d7c04fc);
        assert_eq!(q.y.to_bits(), 0xbfd776fed1bfa573);
        assert_eq!(q.z.to_bits(), 0x3fd01715221d03bd);
        assert_eq!(q.w.to_bits(), 0x3fec2864fbb2c68b);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "axis norm must be finite and > 0")]
    fn from_axis_angle_traps_on_zero_axis() {
        let _ = DQuat::from_axis_angle(DVec3::ZERO, PI);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "quaternion norm must be finite and > 0")]
    fn normalize_traps_on_zero_quaternion() {
        let _ = DQuat::new(0.0, 0.0, 0.0, 0.0).normalize();
    }
}
