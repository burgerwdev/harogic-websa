//! The complex sample type used throughout the chain.

use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

/// A complex sample with `f64` components. Only the operations the chain uses are defined,
/// so the surface stays small and the arithmetic explicit.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cplx {
    pub re: f64,
    pub im: f64,
}

impl Cplx {
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub const fn zero() -> Self {
        Self { re: 0.0, im: 0.0 }
    }

    pub fn from_polar(r: f64, radians: f64) -> Self {
        let (s, c) = radians.sin_cos();
        Self { re: r * c, im: r * s }
    }

    pub const fn conj(self) -> Self {
        Self { re: self.re, im: -self.im }
    }

    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    pub fn norm(self) -> f64 {
        self.norm_sqr().sqrt()
    }

    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
}

impl Add for Cplx {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.re + o.re, self.im + o.im)
    }
}

impl AddAssign for Cplx {
    fn add_assign(&mut self, o: Self) {
        self.re += o.re;
        self.im += o.im;
    }
}

impl Sub for Cplx {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.re - o.re, self.im - o.im)
    }
}

impl SubAssign for Cplx {
    fn sub_assign(&mut self, o: Self) {
        self.re -= o.re;
        self.im -= o.im;
    }
}

impl Neg for Cplx {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.re, -self.im)
    }
}

impl Mul for Cplx {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        Self::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

impl Mul<f64> for Cplx {
    type Output = Self;
    fn mul(self, s: f64) -> Self {
        Self::new(self.re * s, self.im * s)
    }
}

impl Div<f64> for Cplx {
    type Output = Self;
    fn div(self, s: f64) -> Self {
        Self::new(self.re / s, self.im / s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_follows_the_complex_rules() {
        let a = Cplx::new(1.0, 2.0);
        let b = Cplx::new(3.0, -1.0);
        assert_eq!(a + b, Cplx::new(4.0, 1.0));
        assert_eq!(a * b, Cplx::new(5.0, 5.0));
        assert_eq!(a.conj(), Cplx::new(1.0, -2.0));
        assert_eq!(a.norm_sqr(), 5.0);
        assert!((a.norm() - 5.0f64.sqrt()).abs() < 1e-12);
        let p = Cplx::from_polar(2.0, core::f64::consts::FRAC_PI_2);
        assert!(p.re.abs() < 1e-12 && (p.im - 2.0).abs() < 1e-12);
    }
}
