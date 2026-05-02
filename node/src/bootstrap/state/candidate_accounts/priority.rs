use core::ops::{ Add, Deref, Div, Mul, Sub };

#[derive(PartialEq, PartialOrd, Default, Clone, Copy)]
pub struct Priority(f64);

impl Eq for Priority {}
impl Ord for Priority {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(core::cmp::Ordering::Equal)
    }
}

impl Priority {
    pub const fn new(value: f64) -> Self {
        Self(value)
    }

    pub const ZERO: Self = Self(0.0);

    pub fn as_f64(&self) -> f64 {
        self.0
    }
}

impl Add for Priority {
    type Output = Priority;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Priority {
    type Output = Priority;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Mul<f64> for Priority {
    type Output = Priority;

    fn mul(self, rhs: f64) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl Div<f64> for Priority {
    type Output = Priority;

    fn div(self, rhs: f64) -> Self::Output {
        Self(self.0 / rhs)
    }
}

impl From<Priority> for f64 {
    fn from(value: Priority) -> Self {
        value.as_f64()
    }
}

impl core::fmt::Debug for Priority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

impl core::fmt::Display for Priority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

/// Priority in descending order
#[derive(PartialEq, Eq, Default, Clone, Copy)]
pub(crate) struct PriorityKeyDesc(pub Priority);

impl Ord for PriorityKeyDesc {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // order descending
        other.0.cmp(&self.0)
    }
}

impl PartialOrd for PriorityKeyDesc {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Deref for PriorityKeyDesc {
    type Target = Priority;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Priority> for PriorityKeyDesc {
    fn from(value: Priority) -> Self {
        Self(value)
    }
}
