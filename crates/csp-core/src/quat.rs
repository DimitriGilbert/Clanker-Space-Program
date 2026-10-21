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
    /// internally; a zero axis yields NaN — callers validate geometry at
    /// the same boundary where they reject non-finite fields.
    pub fn from_axis_angle(axis: DVec3, angle: f64) -> Self {
        let n = axis.norm();
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
    pub fn normalize(self) -> Self {
        let n = self.norm();
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
}
