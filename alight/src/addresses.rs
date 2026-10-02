use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::ops::{Add, Neg, Sub};

#[derive(Debug, Eq, Default, PartialEq, Clone, Copy, PartialOrd, Ord)]
pub struct Lba(pub i32);

impl Lba {
    pub const PREGAP_START: Lba = Lba(-150);
}

impl Display for Lba {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Eq, Default, PartialEq, Clone, Copy)]
pub struct Msf {
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
}

impl Msf {
    pub fn new(minutes: u8, seconds: u8, frames: u8) -> Msf {
        Msf {
            minutes,
            seconds,
            frames,
        }
    }

    pub fn minutes(minutes: u8) -> Msf {
        Msf::new(minutes, 0, 0)
    }

    pub fn seconds(seconds: u8) -> Msf {
        Msf::new(0, seconds, 0)
    }

    pub fn frames(frames: u8) -> Msf {
        Msf::new(0, 0, frames)
    }
}

impl Display for Msf {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.minutes, self.seconds, self.frames
        )
    }
}

impl From<Msf> for Lba {
    fn from(value: Msf) -> Self {
        Lba(value.frames as i32 + value.seconds as i32 * 75 + value.minutes as i32 * 60 * 75 - 150)
    }
}

impl From<Lba> for Msf {
    fn from(value: Lba) -> Self {
        let adjusted = (value.0 + 150).rem_euclid(450000);
        Msf {
            minutes: (adjusted / (75 * 60)) as u8,
            seconds: ((adjusted / 75) % 60) as u8,
            frames: (adjusted % 75) as u8,
        }
    }
}

impl Add<Lba> for Lba {
    type Output = Lba;

    fn add(self, rhs: Lba) -> Self::Output {
        self + rhs.0
    }
}

impl Sub<Lba> for Lba {
    type Output = Lba;

    fn sub(self, rhs: Lba) -> Self::Output {
        self - rhs.0
    }
}

impl Add<Msf> for Lba {
    type Output = Lba;

    fn add(self, rhs: Msf) -> Self::Output {
        Lba(self.0 + Lba::from(rhs).0)
    }
}

impl Add<Msf> for Msf {
    type Output = Msf;

    fn add(self, rhs: Msf) -> Self::Output {
        let lhs: Lba = self.into();
        let rhs: Lba = rhs.into();
        let sum: Lba = lhs + rhs;
        sum.into()
    }
}

impl Add<Lba> for Msf {
    type Output = Msf;

    fn add(self, rhs: Lba) -> Self::Output {
        (Lba::from(self) + rhs).into()
    }
}

impl Sub<Lba> for Msf {
    type Output = Msf;

    fn sub(self, rhs: Lba) -> Self::Output {
        (Lba::from(self) - rhs).into()
    }
}

impl Add<i32> for Lba {
    type Output = Lba;

    fn add(self, rhs: i32) -> Self::Output {
        Lba(self.0 + rhs)
    }
}

impl Sub<i32> for Lba {
    type Output = Lba;

    fn sub(self, rhs: i32) -> Self::Output {
        Lba(self.0.checked_sub(rhs).unwrap_or_else(|| 450000 - self.0 - rhs))
    }
}

impl Sub<Msf> for Lba {
    type Output = Lba;

    fn sub(self, rhs: Msf) -> Self::Output {
        self - Lba::from(rhs)
    }
}

impl Neg for Lba {
    type Output = Lba;

    fn neg(self) -> Self::Output {
        Lba(-self.0)
    }
}

impl PartialOrd<Msf> for Msf {
    fn partial_cmp(&self, other: &Msf) -> Option<Ordering> {
        Lba::from(self.clone()).partial_cmp(&Lba::from(other.clone()))
    }
}
