use crate::scsi::{ScsiDirection, ScsiDriver, ScsiError, SenseData};
use std::ffi::CString;
use std::mem::offset_of;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tracing::{debug, error};
use windows::Win32::Devices::Cdrom::{
    CDROM_EXCLUSIVE_ACCESS, CDROM_EXCLUSIVE_LOCK, CDROM_LOCK_IGNORE_VOLUME,
    ExclusiveAccessLockDevice, ExclusiveAccessUnlockDevice, IOCTL_CDROM_EXCLUSIVE_ACCESS,
};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_SUCCESS,
};
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, IO_CDROM_EXCLUSIVE_LOCK,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileA, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Storage::IscsiDisc::{
    IOCTL_SCSI_PASS_THROUGH_DIRECT, SCSI_IOCTL_DATA_IN, SCSI_IOCTL_DATA_OUT,
    SCSI_IOCTL_DATA_UNSPECIFIED, SCSI_PASS_THROUGH_DIRECT,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    GUID_DEVINTERFACE_CDROM, IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER,
};
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

    lock_state: Mutex<bool>,
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
        let resolved_path = resolve_device_interface_path(block_device)?;
        let file_handle = open_file_handle(&resolved_path)?;

        let mut driver = WindowsScsiDriver {
            device: block_device.to_string(),
            file_handle,
            vendor: String::new(),
            product: String::new(),
            revision: String::new(),
            lock_state: Mutex::new(false),
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

const LOCK_CALLER_NAME: &[u8] = c"alight".to_bytes_with_nul();

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

    fn take_exclusive_lock(&self) -> Result<(), ScsiError> {
        let mut lock_state = self.lock_state.lock().unwrap();
        if *lock_state {
            return Ok(());
        }

        let mut caller_name = [0_u8; 64];
        caller_name[..LOCK_CALLER_NAME.len()].copy_from_slice(LOCK_CALLER_NAME);

        let lock = CDROM_EXCLUSIVE_LOCK {
            Access: CDROM_EXCLUSIVE_ACCESS {
                RequestType: ExclusiveAccessLockDevice,
                Flags: CDROM_LOCK_IGNORE_VOLUME,
            },
            CallerName: caller_name,
        };

        unsafe {
            DeviceIoControl(
                self.file_handle.0,
                IOCTL_CDROM_EXCLUSIVE_ACCESS,
                Some(&raw const lock as *const core::ffi::c_void),
                size_of::<CDROM_EXCLUSIVE_LOCK>() as u32,
                None,
                0,
                None,
                None,
            )
                .map_err(|e| std::io::Error::last_os_error())?;
        }
        *lock_state = true;
        Ok(())
    }

    fn release_exclusive_lock(&self) -> Result<(), ScsiError> {
        let mut lock_state = self.lock_state.lock().unwrap();
        if !*lock_state {
            return Ok(());
        }

        let unlock = CDROM_EXCLUSIVE_ACCESS {
            RequestType: ExclusiveAccessUnlockDevice,
            Flags: 0,
        };

        unsafe {
            DeviceIoControl(
                self.file_handle.0,
                IOCTL_CDROM_EXCLUSIVE_ACCESS,
                Some(&raw const unlock as *const core::ffi::c_void),
                size_of::<CDROM_EXCLUSIVE_ACCESS>() as u32,
                None,
                0,
                None,
                None,
            )
                .map_err(|_| std::io::Error::last_os_error())?;
        }
        *lock_state = false;
        Ok(())
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
                Some(&raw mut scsi_payload as *mut core::ffi::c_void),
                size_of::<ScsiPayload>() as u32,
                Some(&raw mut scsi_payload as *mut core::ffi::c_void),
                size_of::<ScsiPayload>() as u32,
                Some(&mut bytes_returned),
                None,
            )
                .map_err(|_| std::io::Error::last_os_error())?;
        }

        if scsi_payload.scsi.ScsiStatus != 0 {
            return Err(ScsiError::DriveError {
                sense_data: if scsi_payload.scsi.SenseInfoLength > 0 {
                    Some(SenseData::new(
                        scsi_payload.sense_data[..scsi_payload.scsi.SenseInfoLength as usize]
                            .to_vec(),
                    ))
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

impl Drop for WindowsScsiDriver {
    fn drop(&mut self) {
        let _ = self.release_exclusive_lock();
    }
}

fn open_file_handle(path: &str) -> Result<WindowsHandle, ScsiError> {
    let path_cstr = CString::new(path).unwrap();
    let path_wstr = PCSTR::from_raw(path_cstr.as_ptr() as *const u8);
    
    Ok(WindowsHandle(
        unsafe {
            CreateFileA(
                path_wstr,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
            .map_err(|_| std::io::Error::last_os_error())?,
    ))
}

fn resolve_device_interface_path(device: &str) -> Result<String, ScsiError> {
    if device.starts_with(r"\\?\") {
        return Ok(device.to_string());
    }
    
    if device.len() == 2 {
        let chars = device.chars().collect::<Vec<_>>();
        if chars[1] == ':' && chars[0].is_ascii_uppercase() {
            return resolve_device_interface_path(&format!(r"\\.\{}", device));
        }
    }

    let device_handle = open_file_handle(device)?;
    let target = device_number(&device_handle)?;

    let mut device_interface_len = 0_u32;
    let result = unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &raw mut device_interface_len,
            &GUID_DEVINTERFACE_CDROM,
            PCWSTR::null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if result != CR_SUCCESS {
        return Err(ScsiError::SystemError(format!(
            "CM_Get_Device_Interface_List_SizeW failed with result {:X}",
            result.0
        )));
    }

    let mut buffer = vec![0_u16; device_interface_len as usize];
    let result = unsafe {
        CM_Get_Device_Interface_ListW(
            &GUID_DEVINTERFACE_CDROM,
            PCWSTR::null(),
            &mut buffer,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if result != CR_SUCCESS {
        return Err(ScsiError::SystemError(format!(
            "CM_Get_Device_Interface_ListW failed with result {:X}",
            result.0
        )));
    }

    // The buffer is a list of NUL-terminated strings, ending with an extra NUL.
    for path in buffer.split(|&c| c == 0).filter(|p| !p.is_empty()) {
        let mut path = path.to_vec();
        path.push(0);
        let path = String::from_utf16_lossy(&path[..path.len() - 1]);

        let Ok(handle) = open_file_handle(&path)
        else {
            continue;
        };

        if let Ok(number) = device_number(&handle)
            && number.DeviceType == target.DeviceType
            && number.DeviceNumber == target.DeviceNumber
        {
            debug!("Resolved {} to {}", device, path);
            return Ok(path);
        }
    }

    Err(ScsiError::SystemError(format!(
        "No CD-ROM device interface found for {device}"
    )))
}

fn device_number(handle: &WindowsHandle) -> Result<STORAGE_DEVICE_NUMBER, ScsiError> {
    let mut number = STORAGE_DEVICE_NUMBER::default();
    let mut bytes_returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&raw mut number as *mut core::ffi::c_void),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut bytes_returned),
            None,
        )
            .map_err(|_| std::io::Error::last_os_error())?;
    }
    Ok(number)
}
