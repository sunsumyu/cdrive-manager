//! USN Journal (Update Sequence Number) real-time listener for Windows.
//!
//! Monitors NTFS volume changes using FSCTL_READ_USN_JOURNAL.
//! Provides file system change events for incremental search index updates.
//!
//! ## USN Event Types Tracked
//! - File creation
//! - File deletion
//! - File modification (size change)
//! - File rename
//!
//! ## Reference
//! - https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn-journal-data-v1
//! - https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn-record-v2

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};

/// A single file system change event detected by the USN Journal listener.
/// Uses FRN (File Reference Number) instead of full paths for accurate tracking.
#[derive(Debug, Clone)]
pub enum UsnEvent {
    /// A new file or directory was created.
    FileCreated {
        frn: String,
        parent_frn: String,
        file_name: String,
        is_directory: bool,
    },
    /// A file or directory was deleted.
    FileDeleted {
        frn: String,
        parent_frn: String,
        file_name: String,
    },
    /// A file's content or metadata was modified (size change).
    FileModified {
        frn: String,
        parent_frn: String,
        file_name: String,
    },
    /// A file or directory was renamed.
    FileRenamed {
        old_frn: String,
        new_frn: String,
        parent_frn: String,
        file_name: String,
    },
}

/// Configuration for the USN Journal listener.
pub struct UsnListenerConfig {
    /// The drive letter to monitor (e.g., 'C').
    pub drive_letter: char,
    /// Cancel flag to stop the listener.
    pub cancel_flag: Arc<AtomicBool>,
}

/// Handle to receive USN Journal events.
pub struct UsnListenerHandle {
    pub receiver: Receiver<UsnEvent>,
}

/// Start the USN Journal listener in a background thread.
/// Returns a handle to receive events.
pub fn spawn_usn_listener(config: UsnListenerConfig) -> UsnListenerHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = config.cancel_flag;

    std::thread::spawn(move || {
        if let Err(e) = run_usn_listener(config.drive_letter, sender.clone(), cancel_flag) {
            let _ = sender.send(UsnEvent::FileCreated {
                frn: "0".to_string(),
                parent_frn: "0".to_string(),
                file_name: format!("USN listener error: {}", e),
                is_directory: false,
            });
        }
    });

    UsnListenerHandle { receiver }
}

#[cfg(windows)]
fn run_usn_listener(
    drive_letter: char,
    sender: Sender<UsnEvent>,
    cancel_flag: Arc<AtomicBool>,
) -> Result<()> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use winapi::ctypes::c_void;
    use winapi::shared::minwindef::{DWORD, FALSE};
    use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::ioapiset::DeviceIoControl;
    use winapi::um::winbase::FILE_FLAG_BACKUP_SEMANTICS;
    use winapi::um::winnt::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, HANDLE};

    // USN constants
    const FSCTL_QUERY_USN_JOURNAL: DWORD = 0x000900f4;
    const FSCTL_READ_USN_JOURNAL: DWORD = 0x000900bb;
    const USN_REASON_FILE_CREATE: u32 = 0x00000100;
    const USN_REASON_FILE_DELETE: u32 = 0x00000200;
    const USN_REASON_DATA_OVERWRITE: u32 = 0x00000001;
    const USN_REASON_DATA_EXTENT: u32 = 0x00000002;
    const USN_REASON_RENAME_OLD_NAME: u32 = 0x00001000;
    const USN_REASON_RENAME_NEW_NAME: u32 = 0x00002000;
    const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x00008000;

    /// USN Journal query structure
    #[repr(C)]
    #[derive(Default, Copy, Clone)]
    struct UsnJournalData {
        usn_journal_id: u64,
        first_usn: i64,
        next_usn: i64,
        lowest_valid_usn: i64,
        max_usn: i64,
        maximum_size: u64,
        allocation_delta: u64,
    }

    /// Input structure for reading USN Journal
    #[repr(C)]
    struct ReadUsnJournalData {
        start_usn: i64,
        reason_mask: u32,
        return_only_on_close: u32,
        timeout: u64,
        bytes_to_wait_for: u64,
        usn_journal_id: u64,
    }

    /// USN record v2
    #[repr(C)]
    struct UsnRecord {
        record_length: u32,
        major_version: u16,
        minor_version: u16,
        file_reference_number: u64,
        parent_file_reference_number: u64,
        usn: i64,
        time_stamp: i64,
        reason: u32,
        source_info: u32,
        security_id: u32,
        file_attributes: u32,
        file_name_length: u16,
        file_name_offset: u16,
    }

    // Open the volume handle
    let volume_path = format!("{}:\\", drive_letter);
    let wide_path: Vec<u16> = OsString::from(volume_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let volume_handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };

    if volume_handle.is_null() || volume_handle == winapi::um::handleapi::INVALID_HANDLE_VALUE {
        return Err(anyhow::anyhow!("Failed to open volume {}:", drive_letter));
    }

    struct HandleWrapper(HANDLE);
    impl Drop for HandleWrapper {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0); }
        }
    }
    let _handle = HandleWrapper(volume_handle);

    // Query USN Journal info
    let mut journal_data: UsnJournalData = unsafe { std::mem::zeroed() };
    let mut bytes_returned: DWORD = 0;

    let query_result = unsafe {
        DeviceIoControl(
            volume_handle,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null_mut(),
            0,
            &mut journal_data as *mut _ as *mut c_void,
            std::mem::size_of::<UsnJournalData>() as DWORD,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if query_result == FALSE {
        return Err(anyhow::anyhow!("Failed to query USN Journal on volume {}:", drive_letter));
    }

    // Read USN Journal records
    let mut read_data = ReadUsnJournalData {
        start_usn: journal_data.first_usn,
        reason_mask: 0xFFFFFFFF,
        return_only_on_close: 0,
        timeout: 0,
        bytes_to_wait_for: 0,
        usn_journal_id: journal_data.usn_journal_id,
    };

    let mut buffer = vec![0u8; 1024 * 1024]; // 1MB buffer

    while !cancel_flag.load(Ordering::Relaxed) {
        let read_result = unsafe {
            DeviceIoControl(
                volume_handle,
                FSCTL_READ_USN_JOURNAL,
                &mut read_data as *mut _ as *mut c_void,
                std::mem::size_of::<ReadUsnJournalData>() as DWORD,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as DWORD,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if read_result == FALSE || bytes_returned < std::mem::size_of::<i64>() as DWORD {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Parse records
        let next_usn = unsafe { *(buffer.as_ptr() as *const i64) };
        let mut offset = std::mem::size_of::<i64>();

        while offset + std::mem::size_of::<UsnRecord>() <= bytes_returned as usize {
            let record = unsafe { &*(buffer.as_ptr().add(offset) as *const UsnRecord) };
            let reason = record.reason;
            let file_name_length = record.file_name_length as usize;
            let file_name_offset = record.file_name_offset as usize;
            let record_length = record.record_length as usize;
            let frn = record.file_reference_number.to_string();
            let parent_frn = record.parent_file_reference_number.to_string();

            if file_name_length > 0 && offset + file_name_offset + file_name_length <= bytes_returned as usize {
                let name_ptr = unsafe { buffer.as_ptr().add(offset + file_name_offset) as *const u16 };
                let name_slice = unsafe { std::slice::from_raw_parts(name_ptr, file_name_length / 2) };
                let file_name = std::ffi::OsString::from_wide(name_slice)
                    .to_string_lossy()
                    .to_string();

                let is_directory = (record.file_attributes & 0x10) != 0;

                if (reason & USN_REASON_FILE_CREATE) != 0 {
                    let _ = sender.send(UsnEvent::FileCreated {
                        frn: frn.clone(),
                        parent_frn: parent_frn.clone(),
                        file_name: file_name.clone(),
                        is_directory,
                    });
                }
                if (reason & USN_REASON_FILE_DELETE) != 0 {
                    let _ = sender.send(UsnEvent::FileDeleted {
                        frn: frn.clone(),
                        parent_frn: parent_frn.clone(),
                        file_name: file_name.clone(),
                    });
                }
                if (reason & (USN_REASON_DATA_OVERWRITE | USN_REASON_DATA_EXTENT | USN_REASON_BASIC_INFO_CHANGE)) != 0 {
                    let _ = sender.send(UsnEvent::FileModified {
                        frn: frn.clone(),
                        parent_frn: parent_frn.clone(),
                        file_name: file_name.clone(),
                    });
                }
                if (reason & USN_REASON_RENAME_NEW_NAME) != 0 {
                    let _ = sender.send(UsnEvent::FileRenamed {
                        old_frn: frn.clone(),
                        new_frn: frn.clone(),
                        parent_frn: parent_frn.clone(),
                        file_name: file_name.clone(),
                    });
                }
            }

            offset += record_length;
        }

        read_data.start_usn = next_usn;
    }

    Ok(())
}

#[cfg(not(windows))]
fn run_usn_listener(
    _drive_letter: char,
    _sender: Sender<UsnEvent>,
    _cancel_flag: Arc<AtomicBool>,
) -> Result<()> {
    // USN Journal is Windows-only
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
