//! NTFS MFT (Master File Table) high-speed scanner
//!
//! This module provides direct MFT reading for extremely fast directory scanning.
//! MFT contains all file metadata in a structured format, allowing us to bypass
//! the normal file system API and read metadata directly.
//!
//! Requirements:
//! - Windows NTFS volume
//! - Administrator privileges
//! - Read access to \\.\X: device

#[cfg(windows)]
pub mod windows_mft {
    use std::path::PathBuf;
    use std::ptr;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use std::time::{Duration, Instant, SystemTime};
    
    use anyhow::{Context, Result};
    use crossbeam_channel::Sender;
    use winapi::ctypes::c_void;
    use winapi::um::fileapi::*;
    use winapi::um::handleapi::*;
    use winapi::um::ioapiset::*;
    use winapi::um::winbase::*;
    use winapi::um::winnt::*;
    use winapi::um::minwinbase::OVERLAPPED;
    use winapi::shared::winerror::ERROR_IO_PENDING;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::shared::ntdef::HANDLE;
    
    use crate::model::{FileRecord, ScanStats, file_extension_label};
    use crate::scanner::{ScanEvent, ScanProgress};

    // FSCTL_GET_MFT_RECORD control code for reading MFT directly
    // This is an undocumented Windows API
    const FSCTL_GET_MFT_RECORD: u32 = 0x000900F0;
    
    /// MFT FILE record signature
    const MFT_RECORD_SIGNATURE: u32 = 0x454C4946; // "FILE"
    
    /// Windows FILETIME epoch: January 1, 1601
    /// Unix epoch: January 1, 1970
    /// Difference: 11644473600 seconds
    const FILETIME_UNIX_DIFFERENCE: u64 = 11644473600;
    
    /// Convert Windows FILETIME (100ns intervals since Jan 1, 1601) to SystemTime
    fn filetime_to_systemtime(filetime: u64) -> Option<SystemTime> {
        if filetime == 0 {
            return None;
        }
        
        // Convert to seconds (divide by 10,000,000)
        let seconds = filetime / 10_000_000;
        let nanoseconds = ((filetime % 10_000_000) * 100) as u32;
        
        // Adjust for Unix epoch difference
        if seconds < FILETIME_UNIX_DIFFERENCE {
            return None;
        }
        let unix_seconds = seconds - FILETIME_UNIX_DIFFERENCE;
        
        // Create SystemTime from Unix epoch
        Some(SystemTime::UNIX_EPOCH + Duration::new(unix_seconds, nanoseconds))
    }
    
    /// MFT record size (typically 1024 bytes)
    const MFT_RECORD_SIZE: usize = 1024;

    /// MFT FILE record structure (simplified)
    #[repr(C)]
    struct MftRecord {
        signature: u32,
        offset_to_fixup_array: u16,
        size_of_fixup_entry: u16,
        lsn: u64,
        sequence_number: u16,
        hard_link_count: u16,
        offset_to_first_attribute: u16,
        flags: u16,
        size_in_use: u32,
        allocated_size: u32,
        base_file_reference: u64,
        next_attribute_id: u16,
        padding: u16,
        mft_record_number: u32,
        fixup_array: [u16; 1], // Variable length
    }

    /// Attribute types in MFT records
    #[repr(u32)]
    enum AttributeType {
        StandardInformation = 0x10,
        AttributeList = 0x20,
        FileName = 0x30,
        ObjectID = 0x40,
        SecurityDescriptor = 0x50,
        VolumeName = 0x60,
        VolumeInformation = 0x70,
        Data = 0x80,
        IndexRoot = 0x90,
        IndexAllocation = 0xA0,
        Bitmap = 0xB0,
        ReparsePoint = 0xC0,
        EAInformation = 0xD0,
        EA = 0xE0,
        PropertySet = 0xF0,
        LoggedUtilityStream = 0x100,
        End = 0xFFFFFFFF,
    }

    /// Attribute header structure
    #[repr(C)]
    struct AttributeHeader {
        attribute_type: u32,
        length: u32,
        non_resident_flag: u8,
        name_length: u8,
        offset_to_name: u16,
        flags: u16,
        attribute_id: u16,
    }

    /// File name attribute (type 0x30)
    #[repr(C)]
    struct FileNameAttribute {
        parent_directory_reference: u64,
        creation_time: u64,
        file_change_time: u64,
        mft_change_time: u64,
        last_access_time: u64,
        allocated_size: u64,
        real_size: u64,
        flags: u32,
        reparse_value: u32,
        file_name_length: u8,
        file_name_namespace: u8,
        file_name: [u16; 1], // Variable length
    }

    /// MFT scanner configuration
    pub struct MftScanConfig {
        pub drive_letter: char,
        pub cancel_flag: Arc<AtomicBool>,
    }

    /// Result from MFT scan
    pub struct MftScanResult {
        pub files: Vec<FileRecord>,
        pub total_size: u64,
        pub file_count: u64,
        pub dir_count: u64,
        pub elapsed: Duration,
    }

    /// Open MFT volume for reading
    fn open_mft_volume(drive_letter: char) -> Result<HANDLE> {
        let device_path = format!("\\\\.\\{}:", drive_letter);
        let wide_path: Vec<u16> = device_path.encode_utf16().chain(std::iter::once(0)).collect();
        
        unsafe {
            let handle = CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            );
            
            if handle == INVALID_HANDLE_VALUE {
                let error = GetLastError();
                anyhow::bail!("Failed to open MFT volume: error code {}", error);
            }
            
            Ok(handle)
        }
    }

    /// Get MFT location and size using FSCTL_GET_MFT_RECORD
    fn get_mft_info(handle: HANDLE) -> Result<(u64, u64)> {
        unsafe {
            let mut bytes_returned: u32 = 0;
            let mut mft_info: [u64; 3] = [0; 3]; // StartLCN, FileSize, Flags
            
            let success = DeviceIoControl(
                handle,
                FSCTL_GET_MFT_RECORD,
                ptr::null_mut(),
                0,
                mft_info.as_mut_ptr() as *mut c_void,
                (std::mem::size_of::<[u64; 3]>()) as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            );
            
            if success == 0 {
                let error = GetLastError();
                anyhow::bail!("Failed to get MFT info: error code {}", error);
            }
            
            Ok((mft_info[0], mft_info[1]))
        }
    }

    /// Read MFT records using overlapped I/O
    fn read_mft_records_overlapped(
        handle: HANDLE,
        start_offset: u64,
        buffer_size: usize,
    ) -> Result<Vec<u8>> {
        unsafe {
            let mut buffer = vec![0u8; buffer_size];
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            // Set offset through the union's s field
            (*overlapped.u.s_mut()).Offset = start_offset as u32;
            (*overlapped.u.s_mut()).OffsetHigh = (start_offset >> 32) as u32;
            
            let mut bytes_read: u32 = 0;
            
            let success = ReadFile(
                handle,
                buffer.as_mut_ptr() as *mut c_void,
                buffer_size as u32,
                &mut bytes_read,
                &mut overlapped,
            );
            
            if success == 0 {
                let error = GetLastError();
                if error == ERROR_IO_PENDING {
                    // Wait for completion
                    let mut bytes_transferred: u32 = 0;
                    let wait_result = GetOverlappedResult(
                        handle,
                        &mut overlapped,
                        &mut bytes_transferred,
                        1, // bWait = TRUE
                    );
                    
                    if wait_result == 0 {
                        let wait_error = GetLastError();
                        anyhow::bail!("Overlapped read failed: error code {}", wait_error);
                    }
                    
                    buffer.truncate(bytes_transferred as usize);
                } else {
                    anyhow::bail!("ReadFile failed: error code {}", error);
                }
            } else {
                buffer.truncate(bytes_read as usize);
            }
            
            Ok(buffer)
        }
    }

    /// Parse MFT record and extract file information
    fn parse_mft_record(record_data: &[u8]) -> Option<MftRecordInfo> {
        if record_data.len() < std::mem::size_of::<MftRecord>() {
            return None;
        }
        
        let record: &MftRecord = unsafe {
            &*(record_data.as_ptr() as *const MftRecord)
        };
        
        if record.signature != MFT_RECORD_SIGNATURE {
            return None;
        }
        
        // Check if record is in use
        if record.flags & 0x0001 == 0 {
            return None;
        }
        
        let mut info = MftRecordInfo {
            record_number: record.mft_record_number,
            is_directory: false,
            file_name: None,
            file_size: 0,
            parent_record: 0,
            creation_time: None,
            modification_time: None,
            access_time: None,
            file_attributes: 0,
        };
        
        // Parse attributes
        let mut offset = record.offset_to_first_attribute as usize;
        while offset < record.size_in_use as usize && offset + 4 <= record_data.len() {
            let attr_header: &AttributeHeader = unsafe {
                &*(record_data[offset..].as_ptr() as *const AttributeHeader)
            };
            
            if attr_header.attribute_type == AttributeType::End as u32 {
                break;
            }
            
            if attr_header.length == 0 {
                break;
            }
            
            match attr_header.attribute_type {
                x if x == AttributeType::StandardInformation as u32 => {
                    // Standard Information attribute contains timestamps
                    // Structure: CreationTime, ModificationTime, MftChangeTime, AccessTime (each 8 bytes)
                    // Followed by FileAttributes (4 bytes)
                    let attr_data_offset = offset + std::mem::size_of::<AttributeHeader>();
                    if attr_data_offset + 36 <= record_data.len() {
                        let times_data = &record_data[attr_data_offset..attr_data_offset + 36];
                        
                        // Read timestamps (Windows FILETIME format - 100ns intervals since Jan 1, 1601)
                        info.creation_time = Some(u64::from_le_bytes([
                            times_data[0], times_data[1], times_data[2], times_data[3],
                            times_data[4], times_data[5], times_data[6], times_data[7],
                        ]));
                        info.modification_time = Some(u64::from_le_bytes([
                            times_data[8], times_data[9], times_data[10], times_data[11],
                            times_data[12], times_data[13], times_data[14], times_data[15],
                        ]));
                        // Skip MftChangeTime (bytes 16-23)
                        info.access_time = Some(u64::from_le_bytes([
                            times_data[24], times_data[25], times_data[26], times_data[27],
                            times_data[28], times_data[29], times_data[30], times_data[31],
                        ]));
                        info.file_attributes = u32::from_le_bytes([
                            times_data[32], times_data[33], times_data[34], times_data[35],
                        ]);
                        
                        // Check directory flag from attributes
                        if (info.file_attributes & 0x10) != 0 { // FILE_ATTRIBUTE_DIRECTORY
                            info.is_directory = true;
                        }
                    }
                }
                x if x == AttributeType::FileName as u32 => {
                    if offset + std::mem::size_of::<FileNameAttribute>() <= record_data.len() {
                        let file_name_attr: &FileNameAttribute = unsafe {
                            &*(record_data[offset..].as_ptr() as *const FileNameAttribute)
                        };
                        
                        let name_length = file_name_attr.file_name_length as usize;
                        let name_offset = offset + std::mem::size_of::<FileNameAttribute>() - std::mem::size_of::<u16>();
                        
                        if name_offset + name_length * 2 <= record_data.len() {
                            let name_slice = &record_data[name_offset..name_offset + name_length * 2];
                            let name_utf16: Vec<u16> = name_slice
                                .chunks_exact(2)
                                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                                .collect();
                            
                            if let Some(name) = String::from_utf16(&name_utf16).ok() {
                                // Prefer the first valid filename (usually DOS name or Win32 name)
                                if info.file_name.is_none() {
                                    info.file_name = Some(name);
                                    info.parent_record = (file_name_attr.parent_directory_reference & 0x0000FFFFFFFFFFFF) as u32;
                                    
                                    // Also use size from FileName attribute if available
                                    if file_name_attr.real_size > 0 {
                                        info.file_size = file_name_attr.real_size;
                                    }
                                    
                                    // Check if directory from FileName flags
                                    if (file_name_attr.flags & 0x10000000) != 0 {
                                        info.is_directory = true;
                                    }
                                }
                            }
                        }
                    }
                }
                x if x == AttributeType::Data as u32 => {
                    // For non-resident data, we need to parse the NonResidentAttribute header
                    // which contains the actual file size
                    if attr_header.non_resident_flag != 0 {
                        // Non-resident attribute: size is at offset 32 (after header)
                        let size_offset = offset + 32;
                        if size_offset + 8 <= record_data.len() {
                            let size = u64::from_le_bytes([
                                record_data[size_offset],
                                record_data[size_offset + 1],
                                record_data[size_offset + 2],
                                record_data[size_offset + 3],
                                record_data[size_offset + 4],
                                record_data[size_offset + 5],
                                record_data[size_offset + 6],
                                record_data[size_offset + 7],
                            ]);
                            if size > 0 {
                                info.file_size = size;
                            }
                        }
                    }
                }
                _ => {}
            }
            
            offset += attr_header.length as usize;
        }
        
        Some(info)
    }

    struct MftRecordInfo {
        record_number: u32,
        is_directory: bool,
        file_name: Option<String>,
        file_size: u64,
        parent_record: u32,
        creation_time: Option<u64>,
        modification_time: Option<u64>,
        access_time: Option<u64>,
        file_attributes: u32,
    }

    /// Perform MFT scan with overlapped I/O
    pub fn scan_mft(
        config: MftScanConfig,
        sender: Sender<ScanEvent>,
    ) -> Result<MftScanResult> {
        let scan_start = Instant::now();
        
        // Open MFT volume
        let handle = open_mft_volume(config.drive_letter)
            .context("Failed to open MFT volume")?;
        
        // Get MFT info
        let (mft_start_lcn, mft_size) = get_mft_info(handle)
            .context("Failed to get MFT info")?;
        
        println!("MFT starts at LCN {}, size {} bytes", mft_start_lcn, mft_size);
        
        // Read MFT records in chunks using overlapped I/O
        let chunk_size = 1024 * 1024; // 1MB chunks
        let mut all_records = Vec::new();
        let mut offset = mft_start_lcn * 512; // LCN to byte offset (assuming 512 byte sectors)
        
        while offset < mft_start_lcn * 512 + mft_size {
            if config.cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            
            let bytes_to_read = std::cmp::min(chunk_size, mft_start_lcn * 512 + mft_size - offset) as usize;
            
            match read_mft_records_overlapped(handle, offset, bytes_to_read) {
                Ok(chunk) => {
                    // Parse records in chunk
                    let num_records = chunk.len() / MFT_RECORD_SIZE;
                    for i in 0..num_records {
                        let start = i * MFT_RECORD_SIZE;
                        let end = start + MFT_RECORD_SIZE;
                        
                        if end <= chunk.len() {
                            if let Some(record_info) = parse_mft_record(&chunk[start..end]) {
                                all_records.push(record_info);
                            }
                        }
                    }
                    
                    // Send progress
                    let progress = ScanProgress {
                        stats: Arc::new(ScanStats::default()),
                        current_path: Some(PathBuf::from(format!("MFT offset {}", offset))),
                        finished: false,
                        cancelled: false,
                        estimated_total_dirs: None,
                        estimated_total_files: Some(all_records.len() as u64),
                        scan_mode: crate::scanner::ScanMode::MftScan,
                        active_threads: Some(1),
                    };
                    let _ = sender.send(ScanEvent::Progress(progress));
                }
                Err(e) => {
                    eprintln!("Error reading MFT at offset {}: {}", offset, e);
                    break;
                }
            }
            
            offset += bytes_to_read as u64;
        }
        
        unsafe {
            CloseHandle(handle);
        }
        
        // Convert MFT records to FileRecords
        let mut files = Vec::new();
        let mut total_size = 0u64;
        let mut file_count = 0u64;
        let mut dir_count = 0u64;
        
        for record in &all_records {
            if let Some(name) = &record.file_name {
                if record.is_directory {
                    dir_count += 1;
                } else {
                    file_count += 1;
                    total_size += record.file_size;
                    
                    // Convert modification time from FILETIME to SystemTime
                    let modified = record.modification_time
                        .and_then(filetime_to_systemtime);
                    
                    files.push(FileRecord {
                        path: PathBuf::from(name),
                        size: record.file_size,
                        modified,
                        extension: file_extension_label(&PathBuf::from(name)),
                    });
                }
            }
        }
        
        let elapsed = scan_start.elapsed();
        
        println!(
            "MFT scan completed: {} files, {} dirs, {} bytes in {:?}",
            file_count, dir_count, total_size, elapsed
        );
        
        Ok(MftScanResult {
            files,
            total_size,
            file_count,
            dir_count,
            elapsed,
        })
    }
}

#[cfg(not(windows))]
pub mod windows_mft {
    use anyhow::Result;
    
    pub fn scan_mft() -> Result<()> {
        anyhow::bail!("MFT scanning is only supported on Windows");
    }
}
