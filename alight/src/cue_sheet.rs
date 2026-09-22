use std::fmt::{Debug, Formatter};
use crate::addresses::Msf;

#[derive(Clone, Eq, PartialEq)]
pub struct CueSheetTransition {
    pub control: CueSheetControl,
    pub track: u8,
    pub index: u8,
    pub data_form_subchannel: CueSheetDataFormSubchannel,
    pub data_form: CueSheetDataForm,
    pub scms: u8,
    pub address: Msf
}

pub const TRACK_LEAD_OUT: u8 = 0xAA;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum CueSheetControl {
    Audio = 0x0,

    #[default]
    Data = 0x4
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum CueSheetDataFormSubchannel {
    #[default]
    NoSubchannel = 0x0,
    SupplyRawPQ = 0x1,
    SupplyPackedRW = 0x2,
    SupplyRawRW = 0x3
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum CueSheetDataForm {
    DigitalAudio = 0x0,

    #[default]
    Rom1 = 0x1,
    Rom2 = 0x2
}

impl CueSheetTransition {
    pub fn generate(&self) -> Vec<u8> {
        vec![
            ((self.control as u8) << 4) | 0x1,
            self.track,
            self.index,
            ((self.data_form_subchannel as u8) << 4) | self.data_form as u8,
            self.scms,
            self.address.minutes,
            self.address.seconds,
            self.address.frames
        ]
    }
}

pub struct CueSheet {
    transitions: Vec<CueSheetTransition>,
}

impl CueSheet {
    pub fn new() -> Self {
        Self {
            transitions: Vec::new()
        }
    }

    pub fn generate(&self) -> Vec<u8> {
        self.transitions.iter().flat_map(|t| t.generate()).collect()
    }

    pub fn push_transition(&mut self, transition: CueSheetTransition) {
        self.transitions.push(transition);
    }
}

impl Debug for CueSheet {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "1   2   3   4   5   6   7   8")?;
        for transition in &self.transitions {
            Debug::fmt(transition, f)?;
        }
        Ok(())
    }
}

impl Debug for CueSheetTransition {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for byte in self.generate() {
            write!(f, "{:02x}  ", byte)?;
        }
        writeln!(f)?;
        Ok(())
    }
}