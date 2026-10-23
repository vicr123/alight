use crate::scsi::{ScsiDirection, ScsiDriver, ScsiError, SenseData};
use std::ffi::CString;
use std::mem::offset_of;
use tracing::{debug, error};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileA, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Storage::IscsiDisc::{
    IOCTL_SCSI_PASS_THROUGH_DIRECT, SCSI_IOCTL_DATA_IN, SCSI_IOCTL_DATA_OUT,
    SCSI_IOCTL_DATA_UNSPECIFIED, SCSI_PASS_THROUGH_DIRECT,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::{PCSTR, PCWSTR, w};

struct WindowsHandle(HANDLE);

impl Drop for WindowsHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub struct WindowsScsiDriver {
    device: String,
    file_handle: WindowsHandle,
    vendor: String,
    product: String,
    revision: String,
}

unsafe impl Send for WindowsScsiDriver {}
unsafe impl Sync for WindowsScsiDriver {}

impl From<windows::core::Error> for ScsiError {
    fn from(error: windows::core::Error) -> Self {
        ScsiError::SystemError(error.message())
    }
}

impl WindowsScsiDriver {
    pub fn new(block_device: &str) -> Result<Self, ScsiError> {
        let block_device_cstr = CString::new(block_device).unwrap();
        let block_device_wstr = PCSTR::from_raw(block_device_cstr.as_ptr() as *const u8);
        let file_handle = WindowsHandle(
            unsafe {
                CreateFileA(
                    block_device_wstr,
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_err(|_| std::io::Error::last_os_error())?,
        );

        let mut driver = WindowsScsiDriver {
            device: block_device.to_string(),
            file_handle,
            vendor: String::new(),
            product: String::new(),
            revision: String::new(),
        };

        let _ = driver.rezero();

        let inquiry = driver.inquiry()?;
        driver.vendor = inquiry.vendor;
        driver.product = inquiry.product;
        driver.revision = inquiry.revision;

        let _ = driver.rezero();

        debug!(
            "Windows SCSI driver created: vendor={}, product={}, revision={}",
            driver.vendor, driver.product, driver.revision
        );

        Ok(driver)
    }
}

#[repr(C)]
struct ScsiPayload {
    scsi: SCSI_PASS_THROUGH_DIRECT,
    sense_data: [u8; u8::MAX as usize],
}

impl ScsiDriver for WindowsScsiDriver {
    fn device(&self) -> &str {
        &self.device
    }

    fn vendor(&self) -> &str {
        &self.vendor
    }

    fn product(&self) -> &str {
        &self.product
    }

    fn revision(&self) -> &str {
        &self.revision
    }

    fn send_cmd_with_direction(
        &self,
        cmd: &[u8],
        direction: ScsiDirection,
    ) -> Result<Option<Vec<u8>>, ScsiError> {
        let mut cdb = [0_u8; 16];
        let limit = cmd.len().min(16);
        cdb[..limit].copy_from_slice(&cmd[..limit]);

        let mut dxfer_output = vec![0; 64];

        let mut scsi_payload = ScsiPayload {
            scsi: SCSI_PASS_THROUGH_DIRECT {
                Length: size_of::<SCSI_PASS_THROUGH_DIRECT>() as u16,
                CdbLength: limit as u8,
                DataIn: match direction {
                    ScsiDirection::None => SCSI_IOCTL_DATA_UNSPECIFIED,
                    ScsiDirection::Input(_) => SCSI_IOCTL_DATA_OUT,
                    ScsiDirection::Output => SCSI_IOCTL_DATA_IN,
                } as u8,
                DataTransferLength: match direction {
                    ScsiDirection::None => 0,
                    ScsiDirection::Input(input_buffer) => input_buffer.len() as u32,
                    ScsiDirection::Output => dxfer_output.len() as u32,
                },
                TimeOutValue: 120,
                DataBuffer: match direction {
                    ScsiDirection::None => std::ptr::null_mut(),
                    ScsiDirection::Input(input_buffer) => {
                        &input_buffer[0] as *const u8 as *mut core::ffi::c_void
                    }
                    ScsiDirection::Output => dxfer_output.as_mut_ptr() as *mut core::ffi::c_void,
                },
                SenseInfoLength: u8::MAX,
                SenseInfoOffset: offset_of!(ScsiPayload, sense_data) as u32,
                Cdb: cdb,
                ..Default::default()
            },
            sense_data: [0; u8::MAX as usize],
        };

        let mut bytes_returned: u32 = Default::default();

        unsafe {
            DeviceIoControl(
                self.file_handle.0,
                IOCTL_SCSI_PASS_THROUGH_DIRECT,
                Some(&mut scsi_payload as *mut ScsiPayload as *mut core::ffi::c_void),
                size_of::<ScsiPayload>() as u32,
                Some(&mut scsi_payload as *mut ScsiPayload as *mut core::ffi::c_void),
                size_of::<ScsiPayload>() as u32,
                Some(&mut bytes_returned),
                None,
            )
            .map_err(|_| std::io::Error::last_os_error())?;
        }

        if scsi_payload.scsi.ScsiStatus != 0 {
            return Err(ScsiError::DriveError {
                sense_data: if scsi_payload.scsi.SenseInfoLength > 0 {
                    Some(SenseData::new(scsi_payload.sense_data[..scsi_payload.scsi.SenseInfoLength as usize].to_vec()))
                } else {
                    None
                },
                cmd: cmd.to_vec(),
            });
        }

        Ok(match direction {
            ScsiDirection::Output => dxfer_output.into(),
            _ => None,
        })
    }
}
