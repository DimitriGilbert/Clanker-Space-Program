//! Symmetric 3×3 `f64` tensor — the inertia-tensor representation the
//! §2.4 Stage-1 weld composes.
//!
//! Only the six unique components are stored (`xy = yx` etc.), so
//! symmetry is a property of the type, not a convention. Every public
//! op is exact IEEE `f64` arithmetic in a fixed summation order —
//! bit-identical output for bit-identical input, as §2.4 requires of
//! the weld.

use serde::{Deserialize, Serialize};

use csp_core::{DQuat, DVec3};

/// A body axis, for primitive inertia constructors whose symmetry axis
/// matters (cylinders).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// Symmetric 3×3 tensor, `kg·m²` when used as an inertia tensor:
/// `[[xx, xy, xz], [xy, yy, yz], [xz, yz, zz]]`.
///
/// An inertia tensor is always expressed *about a stated point* — for
/// [`crate::WeldPart::inertia`] the part's own CoM in part-local
/// frame; for [`crate::WeldedBody::inertia_local`] the vessel CoM in
/// body frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymMat3 {
    pub xx: f64,
    pub yy: f64,
    pub zz: f64,
    pub xy: f64,
    pub xz: f64,
    pub yz: f64,
}

impl SymMat3 {
    /// The zero tensor — the inertia of a point mass about itself.
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);

    pub const fn new(xx: f64, yy: f64, zz: f64, xy: f64, xz: f64, yz: f64) -> Self {
        Self {
            xx,
            yy,
            zz,
            xy,
            xz,
            yz,
        }
    }

    /// `I' = R·I·Rᵀ` for rotation `q` — the tensor expressed in the
    /// rotated frame, about the same point. Summation order is fixed
    /// (`i` then `j` over 0..3), so the result is bit-deterministic.
    pub fn rotate(self, q: DQuat) -> Self {
        // Columns of R = rotated basis vectors.
        let r = [q.rotate(DVec3::X), q.rotate(DVec3::Y), q.rotate(DVec3::Z)];
        let m = [
            [self.xx, self.xy, self.xz],
            [self.xy, self.yy, self.yz],
            [self.xz, self.yz, self.zz],
        ];
        let r = [r[0], r[1], r[2]].map(|v| [v.x, v.y, v.z]);
        let mut out = [[0.0; 3]; 3];
        for a in 0..3 {
            for b in 0..3 {
                let mut s = 0.0;
                for i in 0..3 {
                    for j in 0..3 {
                        s += m[i][j] * r[i][a] * r[j][b];
                    }
                }
                out[a][b] = s;
            }
        }
        Self::new(
            out[0][0], out[1][1], out[2][2], out[0][1], out[0][2], out[1][2],
        )
    }

    /// Parallel-axis shift (Steiner): the tensor of the same mass
    /// distribution expressed about a point displaced by `d` from the
    /// point `self` is about. `I' = I + m·(‖d‖²·E − d⊗d)`.
    pub fn shift(self, mass: f64, d: DVec3) -> Self {
        let d2 = d.norm_squared();
        Self::new(
            self.xx + mass * (d2 - d.x * d.x),
            self.yy + mass * (d2 - d.y * d.y),
            self.zz + mass * (d2 - d.z * d.z),
            self.xy - mass * d.x * d.y,
            self.xz - mass * d.x * d.z,
            self.yz - mass * d.y * d.z,
        )
    }

    /// `I·v` — needed wherever angular momentum `L = I·ω` is formed
    /// (momentum conservation on split/merge, §2.4).
    pub fn mul_vec(self, v: DVec3) -> DVec3 {
        DVec3::new(
            self.xx * v.x + self.xy * v.y + self.xz * v.z,
            self.xy * v.x + self.yy * v.y + self.yz * v.z,
            self.xz * v.x + self.yz * v.y + self.zz * v.z,
        )
    }

    /// `xx + yy + zz`.
    pub fn trace(self) -> f64 {
        self.xx + self.yy + self.zz
    }

    /// Determinant of the full 3×3.
    pub fn det(self) -> f64 {
        self.xx * (self.yy * self.zz - self.yz * self.yz)
            - self.xy * (self.xy * self.zz - self.xz * self.yz)
            + self.xz * (self.xy * self.yz - self.xz * self.yy)
    }

    /// `I⁻¹` via adjugate/`det` — `None` when the tensor is singular or
    /// non-finite. The §2.9 backend needs `I⁻¹·τ` to integrate `ω` and
    /// `I⁻¹·(r×J)` inside the contact solver; a PSD-but-singular tensor
    /// (e.g. a point mass) has no angular response and callers treat
    /// `None` as "no angular update" rather than as an error — mass
    /// properties are already weld-validated.
    pub fn inverse(self) -> Option<Self> {
        let det = self.det();
        if !det.is_finite() || det <= 0.0 {
            return None;
        }
        // Cofactor matrix of a symmetric 3×3 is symmetric.
        let c_xx = self.yy * self.zz - self.yz * self.yz;
        let c_yy = self.xx * self.zz - self.xz * self.xz;
        let c_zz = self.xx * self.yy - self.xy * self.xy;
        let c_xy = self.xz * self.yz - self.xy * self.zz;
        let c_xz = self.xy * self.yz - self.xz * self.yy;
        let c_yz = self.xy * self.xz - self.xx * self.yz;
        Some(Self::new(
            c_xx / det,
            c_yy / det,
            c_zz / det,
            c_xy / det,
            c_xz / det,
            c_yz / det,
        ))
    }

    /// All six components finite — the weld's input gate (§2.13).
    pub fn is_finite(self) -> bool {
        self.xx.is_finite()
            && self.yy.is_finite()
            && self.zz.is_finite()
            && self.xy.is_finite()
            && self.xz.is_finite()
            && self.yz.is_finite()
    }

    /// Positive-semidefinite: every principal minor ≥ 0. A physically
    /// valid inertia tensor about a CoM must satisfy this — the weld
    /// rejects part tensors that do not.
    pub fn is_positive_semidefinite(self) -> bool {
        self.is_finite()
            && self.xx >= 0.0
            && self.yy >= 0.0
            && self.zz >= 0.0
            && self.xx * self.yy - self.xy * self.xy >= 0.0
            && self.xx * self.zz - self.xz * self.xz >= 0.0
            && self.yy * self.zz - self.yz * self.yz >= 0.0
            && self.det() >= 0.0
    }

    /// Positive-definite: Sylvester's criterion on leading minors
    /// (`xx > 0`, top-left 2×2 `> 0`, `det > 0`). A rigid body needs a
    /// PD tensor to integrate attitude — test-side sanity property.
    pub fn is_positive_definite(self) -> bool {
        self.xx > 0.0 && self.xx * self.yy - self.xy * self.xy > 0.0 && self.det() > 0.0
    }

    /// Inertia of a solid cuboid of mass `m` and half-extents `h` about
    /// its CoM, axes aligned with the box: `Ixx = m/3·(h_y² + h_z²)`.
    pub fn solid_box(m: f64, h: DVec3) -> Self {
        let k = m / 3.0;
        Self::new(
            k * (h.y * h.y + h.z * h.z),
            k * (h.x * h.x + h.z * h.z),
            k * (h.x * h.x + h.y * h.y),
            0.0,
            0.0,
            0.0,
        )
    }

    /// Inertia of a solid sphere of mass `m`, radius `r`, about its
    /// center: `I = 2/5·m·r²` on the diagonal.
    pub fn solid_sphere(m: f64, r: f64) -> Self {
        let i = 0.4 * m * r * r;
        Self::new(i, i, i, 0.0, 0.0, 0.0)
    }

    /// Inertia of a solid cylinder of mass `m`, radius `r`, half-height
    /// `h`, symmetry axis `axis`, about its CoM: `I_axis = ½·m·r²`,
    /// `I_⊥ = m/12·(3r² + (2h)²)`.
    pub fn solid_cylinder(m: f64, r: f64, h: f64, axis: Axis) -> Self {
        let axial = 0.5 * m * r * r;
        let transverse = m * (3.0 * r * r + 4.0 * h * h) / 12.0;
        match axis {
            Axis::X => Self::new(axial, transverse, transverse, 0.0, 0.0, 0.0),
            Axis::Y => Self::new(transverse, axial, transverse, 0.0, 0.0, 0.0),
            Axis::Z => Self::new(transverse, transverse, axial, 0.0, 0.0, 0.0),
        }
    }
}

impl core::ops::Add for SymMat3 {
    type Output = Self;
    /// `self + o`, componentwise.
    fn add(self, o: Self) -> Self {
        Self::new(
            self.xx + o.xx,
            self.yy + o.yy,
            self.zz + o.zz,
            self.xy + o.xy,
            self.xz + o.xz,
            self.yz + o.yz,
        )
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use core::f64::consts::FRAC_PI_2;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-14 * b.abs().max(1.0)
    }

    #[test]
    fn shift_is_parallel_axis() {
        // Point mass m at distance d from the pivot: I = m·d² ⊥ to d.
        let i = SymMat3::ZERO.shift(2.0, DVec3::new(3.0, 0.0, 0.0));
        assert_eq!(i.xx, 0.0);
        assert_eq!(i.yy, 18.0);
        assert_eq!(i.zz, 18.0);
        // Off-axis displacement produces products of inertia.
        let i = SymMat3::ZERO.shift(4.0, DVec3::new(0.5, 0.25, 0.0));
        assert!(close(i.xy, -4.0 * 0.5 * 0.25));
    }

    #[test]
    fn rotate_swaps_axes_on_quarter_turn() {
        let i = SymMat3::new(1.0, 2.0, 3.0, 0.0, 0.0, 0.0);
        let q = DQuat::from_axis_angle(DVec3::Z, FRAC_PI_2);
        let r = i.rotate(q);
        // 90° about z swaps x and y.
        assert!(close(r.xx, 2.0));
        assert!(close(r.yy, 1.0));
        assert!(close(r.zz, 3.0));
    }

    #[test]
    fn rotate_preserves_trace_and_det() {
        let i = SymMat3::new(5.0, 3.0, 2.0, 0.5, -0.25, 0.75);
        let q = DQuat::from_axis_angle(DVec3::new(1.0, -2.0, 0.5), 1.1);
        let r = i.rotate(q);
        assert!(close(r.trace(), i.trace()));
        assert!(close(r.det(), i.det()));
    }

    #[test]
    fn solid_primitives_are_positive_definite() {
        assert!(SymMat3::solid_box(12.0, DVec3::new(0.5, 0.25, 1.0)).is_positive_definite());
        assert!(SymMat3::solid_sphere(3.0, 0.75).is_positive_definite());
        assert!(SymMat3::solid_cylinder(8.0, 0.5, 1.5, Axis::Y).is_positive_definite());
        // A point mass is PSD but not PD.
        assert!(SymMat3::ZERO.is_positive_semidefinite());
        assert!(!SymMat3::ZERO.is_positive_definite());
        // Garbage is rejected.
        assert!(!SymMat3::new(-1.0, 1.0, 1.0, 0.0, 0.0, 0.0).is_positive_semidefinite());
        assert!(!SymMat3::new(1.0, 1.0, 1.0, 2.0, 0.0, 0.0).is_positive_semidefinite());
    }

    /// `I⁻¹·I = E` componentwise, and singular/PSD tensors return `None`.
    #[test]
    fn inverse_round_trips_and_rejects_singular() {
        let i = SymMat3::new(5.0, 3.0, 2.0, 0.5, -0.25, 0.75);
        let inv = i.inverse().unwrap();
        // (I⁻¹·I)·v = v on a probe vector.
        let probe = DVec3::new(1.5, -0.75, 2.25);
        let back = inv.mul_vec(i.mul_vec(probe));
        assert_vec_close(back, probe);
        // Diagonal sanity: 1/5, 1/3, 1/2.
        let d = SymMat3::new(5.0, 3.0, 2.0, 0.0, 0.0, 0.0)
            .inverse()
            .unwrap();
        assert!(close(d.xx, 0.2) && close(d.yy, 1.0 / 3.0) && close(d.zz, 0.5));
        assert!(SymMat3::ZERO.inverse().is_none());
        assert!(
            SymMat3::new(f64::NAN, 1.0, 1.0, 0.0, 0.0, 0.0)
                .inverse()
                .is_none()
        );
    }

    fn assert_vec_close(a: DVec3, b: DVec3) {
        assert!(
            close(a.x, b.x) && close(a.y, b.y) && close(a.z, b.z),
            "expected {b:?}, got {a:?}"
        );
    }

    #[test]
    fn cylinder_axis_assigns_axial_term() {
        let i = SymMat3::solid_cylinder(10.0, 0.5, 2.0, Axis::Y);
        assert!(close(i.yy, 0.5 * 10.0 * 0.25));
        let t = 10.0 * (3.0 * 0.25 + 4.0 * 4.0) / 12.0;
        assert!(close(i.xx, t));
        assert!(close(i.zz, t));
    }
}
