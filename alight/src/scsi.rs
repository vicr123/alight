use crate::driver::CdrDriverError;
use crate::scsi::linux::LinuxScsiDriver;
use std::fmt::{Display, Formatter};
use std::io::Error;
use std::ops::{Deref, Index};

mod linux;

pub enum ScsiOpcode {
    Rezero = 0x01,
    StartStopUnit = 0x1B,
    PreventAllowMediumRemoval = 0x1E,
    Write10 = 0x2A,
    FlushCache = 0x35,
    ReadDiskInfo = 0x51,
    ReadTrackInformation = 0x52,
    SendOpcInformation = 0x54,
    ModeSelect = 0x55,
    ModeSense = 0x5A,
    ReadBufferCapacity = 0x5C,
    SendCueSheet = 0x5D,
    Blank = 0xA1,
    SetCdSpeed = 0xBB,
}

pub enum ScsiDirection<'a> {
    None,
    Input(&'a [u8]),
    Output,
}

#[derive(Debug)]
pub enum ScsiError {
    IoError(std::io::Error),
    DriveError {
        cmd: Vec<u8>,
        sense_data: Option<SenseData>,
    },
}

#[derive(Debug, Clone)]
pub struct SenseData {
    pub bytes: Vec<u8>,
}

impl SenseData {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl Index<usize> for SenseData {
    type Output = u8;

    fn index(&self, index: usize) -> &Self::Output {
        &self.bytes[index]
    }
}

impl From<std::io::Error> for ScsiError {
    fn from(value: Error) -> Self {
        ScsiError::IoError(value)
    }
}

impl Display for ScsiError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ScsiError::IoError(e) => Display::fmt(e, f),
            ScsiError::DriveError { cmd, sense_data } => {
                write!(f, "Generic SCSI error")?;
                if let Some(sense_data) = sense_data {
                    write!(f, ": Sense bytes:")?;
                    for sense_byte in sense_data.bytes() {
                        write!(f, " {:02X}", sense_byte)?;
                    }
                };
                write!(f, ". Original command:")?;
                for cmd_byte in cmd {
                    write!(f, " {:02X}", cmd_byte)?;
                }
                Ok(())
            }
        }
    }
}

pub struct ScsiEnquiry {
    pub vendor: String,
    pub product: String,
    pub revision: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TestUnitReadyResponse {
    Ready,
    Busy,
    NoMedia,
}

impl From<TestUnitReadyResponse> for bool {
    fn from(value: TestUnitReadyResponse) -> Self {
        value == TestUnitReadyResponse::Ready
    }
}

pub trait ScsiDriver: Send + Sync {
    fn device(&self) -> &str;

    fn vendor(&self) -> &str;
    fn product(&self) -> &str;
    fn revision(&self) -> &str;

    fn send_cmd_with_direction(
        &self,
        cmd: &[u8],
        direction: ScsiDirection,
    ) -> Result<Option<Vec<u8>>, ScsiError>;

    fn send_cmd(&self, cmd: &[u8]) -> Result<(), ScsiError> {
        self.send_cmd_with_direction(cmd, ScsiDirection::None)?;
        Ok(())
    }

    fn send_cmd_with_input(&self, cmd: &[u8], input: &[u8]) -> Result<(), ScsiError> {
        self.send_cmd_with_direction(cmd, ScsiDirection::Input(input))?;
        Ok(())
    }

    fn send_cmd_with_output(&self, cmd: &[u8]) -> Result<Vec<u8>, ScsiError> {
        self.send_cmd_with_direction(cmd, ScsiDirection::Output)
            .map(|output| output.expect("Sent SCSI command but received no output"))
    }

    fn inquiry(&self) -> Result<ScsiEnquiry, ScsiError> {
        let output = self.send_cmd_with_output(&[0x12, 0x0, 0x0, 0x0, 0x2C, 0x0])?;

        Ok(ScsiEnquiry {
            vendor: String::from_utf8_lossy(&output[8..16]).to_string(),
            product: String::from_utf8_lossy(&output[16..32]).to_string(),
            revision: String::from_utf8_lossy(&output[32..36]).to_string(),
        })
    }

    fn test_unit_ready(&self) -> Result<TestUnitReadyResponse, ScsiError> {
        match self.send_cmd(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0]) {
            Ok(_) => Ok(TestUnitReadyResponse::Ready),
            Err(ScsiError::DriveError {
                sense_data: Some(sense_data),
                cmd,
            }) => match sense_data[2] & 0x0F {
                // Not Ready
                0x02 => match sense_data[12] {
                    0x3A => Ok(TestUnitReadyResponse::NoMedia),
                    _ => Ok(TestUnitReadyResponse::Busy),
                },

                // Unit Attention
                0x06 => Ok(TestUnitReadyResponse::Ready),
                _ => Err(ScsiError::DriveError {
                    sense_data: Some(sense_data),
                    cmd,
                }),
            },
            Err(e) => Err(e),
        }
    }

    fn rezero(&self) -> Result<(), ScsiError> {
        self.send_cmd(&[ScsiOpcode::Rezero as u8, 0x0, 0x0, 0x0, 0x0, 0x0])
    }
}

pub fn create_scsi_driver<'a, 'b>(file: &'a str) -> Result<impl ScsiDriver + 'b, ScsiError> {
    LinuxScsiDriver::new(file)
}
