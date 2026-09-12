use std::io::Error;
use crate::scsi::linux::LinuxScsiDriver;

mod linux;

pub enum ScsiDirection<'a> {
    None,
    Input(&'a [u8]),
    Output,
}

#[derive(Debug)]
pub enum ScsiError {
    IoError(std::io::Error),
    DriveError {
        sense_data: Option<Vec<u8>>,
    }
}

impl From<std::io::Error> for ScsiError {
    fn from(value: Error) -> Self {
        ScsiError::IoError(value)
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
            }) => match sense_data[2] & 0x0F {
                // Not Ready
                0x02 => match sense_data[12] {
                    0x3A => Ok(TestUnitReadyResponse::NoMedia),
                    _ => Ok(TestUnitReadyResponse::Busy),
                },

                // Unit Attention
                0x06 => Ok(TestUnitReadyResponse::Ready),
                _ => Err(ScsiError::DriveError {
                    sense_data: Some(sense_data)
                }),
            },
            Err(e) => Err(e),
        }
    }
    
    fn rezero(&self) -> Result<(), ScsiError> {
        self.send_cmd(&[0x01, 0x0, 0x0, 0x0, 0x0, 0x0])
    }
}

pub fn create_scsi_driver<'a, 'b>(file: &'a str) -> Result<impl ScsiDriver + 'b, ScsiError> {
    LinuxScsiDriver::new(file)
}
