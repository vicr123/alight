use std::fmt::{Display, Formatter};
use std::ops::Add;

#[derive(Debug, Eq, Default, PartialEq, Clone, Copy)]
pub struct Lba(pub u64);

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
        Lba(value.frames as u64 + value.seconds as u64 * 75 + value.minutes as u64 * 60 * 75)
    }
}

impl From<Lba> for Msf {
    fn from(value: Lba) -> Self {
        Msf {
            minutes: (value.0 / (75 * 60)) as u8,
            seconds: ((value.0 / 75) % 60) as u8,
            frames: (value.0 % 75) as u8,
        }
    }
}

impl Add<Lba> for Lba {
    type Output = Lba;

    fn add(self, rhs: Lba) -> Self::Output {
        Lba(self.0 + rhs.0)
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
