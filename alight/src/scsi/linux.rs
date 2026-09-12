use crate::scsi::{ScsiDirection, ScsiDriver, ScsiError};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use tracing::{debug, error};

#[repr(C)]
#[derive(Default)]
struct SgIoHdr {
    interface_id: i32,
    dxfer_direction: i32,
    cmd_len: u8,
    mx_sb_len: u8,
    iovec_count: u16,
    dxfer_len: u32,
    dxferp: *mut core::ffi::c_void,
    cmdp: *mut u8,
    sbp: *mut u8,
    timeout: u32,
    flags: u32,
    pack_id: i32,
    usr_ptr: *mut core::ffi::c_void,
    status: u8,
    masked_status: u8,
    msg_status: u8,
    sb_len_wr: u8,
    host_status: u16,
    driver_status: u16,
    resid: i32,
    duration: u32,
    info: u32,
}

const SG_DXFER_NONE: i32 = -1;
const SG_DXFER_TO_DEV: i32 = -2;
const SG_DXFER_FROM_DEV: i32 = -3;

const SG_IO: libc::c_ulong = 0x2285;

struct LinuxFd(RawFd);

impl LinuxFd {
    pub fn new(block_device: &str) -> Result<Self, ScsiError> {
        let c_path = CString::new(block_device).map_err(|_| ScsiError::DriveError {
            sense_data: None
        })?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_EXCL) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            error!("Unable to open block device: {}", e);
            return Err(e.into());
        }
        Ok(LinuxFd(fd))
    }
}

impl Drop for LinuxFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub struct LinuxScsiDriver {
    device: String,
    fd: LinuxFd,
    vendor: String,
    product: String,
    revision: String,
}

impl LinuxScsiDriver {
    pub fn new(block_device: &str) -> Result<Self, ScsiError> {
        let fd = LinuxFd::new(block_device)?;
        let mut driver = LinuxScsiDriver {
            device: block_device.to_string(),
            fd,
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
            "Linux SCSI driver created: vendor={}, product={}, revision={}",
            driver.vendor, driver.product, driver.revision
        );

        Ok(driver)
    }
}

impl ScsiDriver for LinuxScsiDriver {
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
        if cmd.len() > u8::MAX as usize {
            panic!("Command length exceeds maximum allowed value")
        };

        let sense = vec![0u8; u8::MAX as usize];

        let mut dxfer_output = vec![0; 64];

        let mut io_hdr = SgIoHdr {
            interface_id: 'S' as i32,
            cmd_len: cmd.len() as u8,
            cmdp: &cmd[0] as *const u8 as *mut u8,
            timeout: 10000,
            sbp: &sense[0] as *const u8 as *mut u8,
            mx_sb_len: sense.len() as u8,
            flags: 1,
            dxferp: match direction {
                ScsiDirection::None => std::ptr::null_mut(),
                ScsiDirection::Input(input_buffer) => {
                    &input_buffer[0] as *const u8 as *mut core::ffi::c_void
                }
                ScsiDirection::Output => dxfer_output.as_mut_ptr() as *mut core::ffi::c_void,
            },
            dxfer_len: match direction {
                ScsiDirection::None => 0,
                ScsiDirection::Input(input_buffer) => input_buffer.len() as u32,
                ScsiDirection::Output => dxfer_output.len() as u32,
            },
            dxfer_direction: match direction {
                ScsiDirection::None => SG_DXFER_NONE,
                ScsiDirection::Input(_) => SG_DXFER_TO_DEV,
                ScsiDirection::Output => SG_DXFER_FROM_DEV,
            },
            ..Default::default()
        };

        let rc = unsafe { libc::ioctl(self.fd.0, SG_IO, &mut io_hdr as *mut SgIoHdr) };

        if rc < 0 {
            let e = std::io::Error::last_os_error();
            return Err(e.into());
        }

        if io_hdr.status != 0 {
            return Err(ScsiError::DriveError {
                sense_data: if io_hdr.sb_len_wr > 0 {
                    Some(sense[..io_hdr.sb_len_wr as usize].to_vec())
                } else {
                    None
                }
            })
        }

        Ok(match direction {
            ScsiDirection::Output => dxfer_output.into(),
            _ => None,
        })
    }
}
