//! Minimal deterministic `f64` 3-vector (PLAN §2.1: `csp-core` owns
//! "f64 math (libm only)"; §2.8's `OrbitModel` speaks in `DVec3`s).
//!
//! Deliberately tiny: component ops, `dot`, `cross`, norms. The only
//! transcendental-adjacent operation is [`DVec3::norm`], which goes through
//! `libm::sqrt` — this crate's `clippy.toml` bans `f64::sqrt` like every
//! other non-`libm` float math entry point (§2.7/§8.2). Everything else is
//! exact IEEE `f64` arithmetic.

use core::ops::{Add, Div, Mul, Neg, Sub};

/// A 3-component `f64` vector — positions (m) and velocities (m/s) in the
/// orbit layer, free of any fixed frame convention (the caller documents
/// which frame a given value lives in).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DVec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl DVec3 {
    /// The zero vector.
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);
    /// Unit +x.
    pub const X: Self = Self::new(1.0, 0.0, 0.0);
    /// Unit +y.
    pub const Y: Self = Self::new(0.0, 1.0, 0.0);
    /// Unit +z.
    pub const Z: Self = Self::new(0.0, 0.0, 1.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// Dot product.
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    /// Cross product (`self × o`).
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    /// Squared Euclidean norm — cheaper than [`Self::norm`], and the form
    /// most energy/vis-viva expressions actually need.
    pub fn norm_squared(self) -> f64 {
        self.dot(self)
    }

    /// Euclidean norm. `libm::sqrt`, never `f64::sqrt` (§2.7).
    pub fn norm(self) -> f64 {
        libm::sqrt(self.norm_squared())
    }

    /// `|self − o|` — the SOI-scan distance measure's operand.
    pub fn distance(self, o: Self) -> f64 {
        (self - o).norm()
    }

    /// All three components finite — the §2.13 sanity gate for anything
    /// about to enter or leave f64 math.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Add for DVec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for DVec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Neg for DVec3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl Mul<f64> for DVec3 {
    type Output = Self;
    fn mul(self, k: f64) -> Self {
        Self::new(self.x * k, self.y * k, self.z * k)
    }
}

impl Mul<DVec3> for f64 {
    type Output = DVec3;
    fn mul(self, v: DVec3) -> DVec3 {
        v * self
    }
}

impl Div<f64> for DVec3 {
    type Output = Self;
    fn div(self, k: f64) -> Self {
        Self::new(self.x / k, self.y / k, self.z / k)
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn component_ops() {
        let a = DVec3::new(1.0, 2.0, 3.0);
        let b = DVec3::new(4.0, -5.0, 6.0);
        assert_eq!(a + b, DVec3::new(5.0, -3.0, 9.0));
        assert_eq!(a - b, DVec3::new(-3.0, 7.0, -3.0));
        assert_eq!(-a, DVec3::new(-1.0, -2.0, -3.0));
        assert_eq!(a * 2.0, DVec3::new(2.0, 4.0, 6.0));
        assert_eq!(2.0 * a, a * 2.0);
        assert_eq!(a / 2.0, DVec3::new(0.5, 1.0, 1.5));
        assert_eq!(a.dot(b), 1.0 * 4.0 + 2.0 * (-5.0) + 3.0 * 6.0);
    }

    #[test]
    fn cross_is_orthogonal_and_antisymmetric() {
        let a = DVec3::new(1.0, 2.0, 3.0);
        let b = DVec3::new(-2.0, 0.5, 4.0);
        let c = a.cross(b);
        assert!(c.dot(a).abs() < 1e-15);
        assert!(c.dot(b).abs() < 1e-15);
        assert_eq!(b.cross(a), -c);
        assert_eq!(DVec3::X.cross(DVec3::Y), DVec3::Z);
    }

    #[test]
    fn norm_is_exact_on_pythagorean() {
        let v = DVec3::new(2.0, 3.0, 6.0); // 4+9+36 = 49
        assert_eq!(v.norm_squared(), 49.0);
        assert_eq!(v.norm(), 7.0);
        assert_eq!(v.distance(DVec3::ZERO), 7.0);
    }

    #[test]
    fn finiteness_check() {
        assert!(DVec3::new(1.0, 2.0, 3.0).is_finite());
        assert!(!DVec3::new(f64::NAN, 0.0, 0.0).is_finite());
        assert!(!DVec3::new(0.0, f64::INFINITY, 0.0).is_finite());
    }
}
