//! NTFS MFT (Master File Table) high-speed scanner
//!
//! This module reads NTFS file records through Windows filesystem control codes.
//! It is intentionally best-effort: if the current process cannot open the
//! volume, or the target volume is not NTFS, the caller falls back to the normal
//! multi-threaded directory scan.
//!
//! Requirements:
//! - Windows NTFS volume
//! - Administrator privileges in most environments
//! - Read access to `\\.\X:`

#[cfg(windows)]
pub mod windows_mft {
    use std::collections::HashMap;
    use std::fmt;
    use std::path::PathBuf;
    use std::ptr;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant, SystemTime};

    use anyhow::{Context, Result};
    use crossbeam_channel::Sender;
    use winapi::ctypes::c_void;
    use winapi::shared::minwindef::{BOOL, FALSE};
    use winapi::shared::ntdef::{HANDLE, NULL};
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::fileapi::*;
    use winapi::um::handleapi::*;
    use winapi::um::ioapiset::DeviceIoControl;
    use winapi::um::securitybaseapi::{AllocateAndInitializeSid, CheckTokenMembership, FreeSid};
    use winapi::um::winnt::*;

    use crate::model::{FileRecord, ScanStats, file_extension_label};
    use crate::scanner::{ScanEvent, ScanProgress};

    const FILE_DEVICE_FILE_SYSTEM: u32 = 0x0000_0009;
    const METHOD_BUFFERED: u32 = 0;
    const FILE_ANY_ACCESS_CTL: u32 = 0;

    const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
        (device_type << 16) | (access << 14) | (function << 2) | method
    }

    const FSCTL_GET_NTFS_VOLUME_DATA: u32 = ctl_code(
        FILE_DEVICE_FILE_SYSTEM,
        25,
        METHOD_BUFFERED,
        FILE_ANY_ACCESS_CTL,
    );
    const FSCTL_GET_NTFS_FILE_RECORD: u32 = ctl_code(
        FILE_DEVICE_FILE_SYSTEM,
        26,
        METHOD_BUFFERED,
        FILE_ANY_ACCESS_CTL,
    );

    const MFT_RECORD_SIGNATURE: u32 = 0x454C_4946; // "FILE"
    const FILETIME_UNIX_DIFFERENCE: u64 = 11_644_473_600;
    const FILE_ATTRIBUTE_DIRECTORY_FLAG: u32 = 0x10;
    const MFT_RECORD_IN_USE: u16 = 0x0001;
    const MFT_RECORD_IS_DIRECTORY: u16 = 0x0002;
    const FILE_REFERENCE_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
    const ROOT_FILE_REFERENCE: u64 = 5;
    const PROGRESS_RECORD_INTERVAL: u64 = 4096;
    const PROGRESS_TIME_INTERVAL: Duration = Duration::from_millis(250);

    fn filetime_to_systemtime(filetime: u64) -> Option<SystemTime> {
        if filetime == 0 {
            return None;
        }

        let seconds = filetime / 10_000_000;
        let nanoseconds = ((filetime % 10_000_000) * 100) as u32;
        if seconds < FILETIME_UNIX_DIFFERENCE {
            return None;
        }

        Some(
            SystemTime::UNIX_EPOCH + Duration::new(seconds - FILETIME_UNIX_DIFFERENCE, nanoseconds),
        )
    }

    #[repr(C)]
    #[derive(Default)]
    struct NtfsVolumeDataBuffer {
        volume_serial_number: i64,
        number_sectors: i64,
        total_clusters: i64,
        free_clusters: i64,
        total_reserved: i64,
        bytes_per_sector: u32,
        bytes_per_cluster: u32,
        bytes_per_file_record_segment: u32,
        clusters_per_file_record_segment: u32,
        mft_valid_data_length: i64,
        mft_start_lcn: i64,
        mft2_start_lcn: i64,
        mft_zone_start: i64,
        mft_zone_end: i64,
    }

    #[repr(C)]
    struct NtfsFileRecordInputBuffer {
        file_reference_number: i64,
    }

    pub struct MftScanConfig {
        pub root: PathBuf,
        pub drive_letter: char,
        pub cancel_flag: Arc<AtomicBool>,
    }

    pub struct MftScanResult {
        pub directories: Vec<PathBuf>,
        pub files: Vec<FileRecord>,
        pub elapsed: Duration,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MftPrivilegeStatus {
        Elevated,
        NotElevated,
    }

    impl fmt::Display for MftPrivilegeStatus {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Elevated => formatter.write_str("管理员权限"),
                Self::NotElevated => formatter.write_str("非管理员权限"),
            }
        }
    }

    pub fn current_privilege_status() -> MftPrivilegeStatus {
        unsafe {
            let mut admin_group: PSID = ptr::null_mut();
            let mut nt_authority = SID_IDENTIFIER_AUTHORITY {
                Value: SECURITY_NT_AUTHORITY,
            };

            let allocated = AllocateAndInitializeSid(
                &mut nt_authority,
                2,
                SECURITY_BUILTIN_DOMAIN_RID,
                DOMAIN_ALIAS_RID_ADMINS,
                0,
                0,
                0,
                0,
                0,
                0,
                &mut admin_group,
            );

            if allocated == FALSE {
                return MftPrivilegeStatus::NotElevated;
            }

            let mut is_member: BOOL = FALSE;
            let checked = CheckTokenMembership(NULL as HANDLE, admin_group, &mut is_member);
            FreeSid(admin_group);

            if checked != FALSE && is_member != FALSE {
                MftPrivilegeStatus::Elevated
            } else {
                MftPrivilegeStatus::NotElevated
            }
        }
    }

    fn windows_error_hint(error: u32) -> &'static str {
        match error {
            5 => "访问被拒绝：请以管理员身份运行程序，再重试 MFT 高速扫描。",
            2 | 3 => "找不到指定卷：请确认扫描路径是存在的驱动器根目录，例如 C:\\。",
            21 => "设备未就绪：请确认目标驱动器已挂载且可访问。",
            32 => "卷正被其他进程独占使用：请关闭磁盘工具或安全软件后重试。",
            87 => "参数不正确：请确认目标是 NTFS 驱动器根目录。",
            50 => "系统不支持该请求：目标可能不是 NTFS 卷。",
            _ => "可先尝试以管理员身份运行；如果仍失败，请确认目标是本机 NTFS 驱动器根目录。",
        }
    }

    fn is_drive_root_path(root: &PathBuf, drive_letter: char) -> bool {
        let expected_drive = drive_letter.to_ascii_uppercase();
        let Some(root_str) = root.to_str() else {
            return false;
        };
        let normalized = root_str.trim().replace('/', "\\");
        let normalized = normalized.trim_end_matches('\\');
        let mut chars = normalized.chars();
        matches!(
            (chars.next(), chars.next(), chars.next()),
            (Some(drive), Some(':'), None) if drive.to_ascii_uppercase() == expected_drive
        )
    }

    fn validate_mft_target(root: &PathBuf, drive_letter: char) -> Result<()> {
        if !is_drive_root_path(root, drive_letter) {
            anyhow::bail!(
                "MFT 扫描只支持驱动器根目录（例如 {}:\\），当前路径为 {}。请切换到驱动器根目录，或使用普通多线程扫描。",
                drive_letter.to_ascii_uppercase(),
                root.display()
            );
        }

        Ok(())
    }

    struct MftRecordInfo {
        record_number: u64,
        is_directory: bool,
        file_name: String,
        file_namespace_score: u8,
        file_size: u64,
        parent_record: u64,
        modification_time: Option<u64>,
    }

    struct MftNode {
        parent_record: u64,
        name: String,
        is_directory: bool,
        size: u64,
        modified: Option<SystemTime>,
    }

    fn open_ntfs_volume(drive_letter: char) -> Result<HANDLE> {
        let device_path = format!("\\\\.\\{}:", drive_letter.to_ascii_uppercase());
        let wide_path: Vec<u16> = device_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            );

            if handle == INVALID_HANDLE_VALUE {
                let error = GetLastError();
                let privilege = current_privilege_status();
                anyhow::bail!(
                    "无法打开卷 {}（Windows 错误码 {}，{}）。{}",
                    device_path,
                    error,
                    privilege,
                    windows_error_hint(error)
                );
            }

            Ok(handle)
        }
    }

    fn get_ntfs_volume_data(handle: HANDLE) -> Result<NtfsVolumeDataBuffer> {
        unsafe {
            let mut data = NtfsVolumeDataBuffer::default();
            let mut bytes_returned: u32 = 0;
            let success = DeviceIoControl(
                handle,
                FSCTL_GET_NTFS_VOLUME_DATA,
                ptr::null_mut(),
                0,
                &mut data as *mut _ as *mut c_void,
                std::mem::size_of::<NtfsVolumeDataBuffer>() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            );

            if success == 0 {
                let error = GetLastError();
                anyhow::bail!(
                    "无法读取 NTFS 卷信息（错误码 {}）。目标可能不是 NTFS 卷。",
                    error
                );
            }

            if data.bytes_per_file_record_segment == 0 || data.bytes_per_sector == 0 {
                anyhow::bail!("NTFS 卷信息无效：文件记录大小或扇区大小为 0");
            }

            Ok(data)
        }
    }

    fn get_ntfs_file_record(
        handle: HANDLE,
        file_reference_number: u64,
        record_size: usize,
    ) -> Result<(u64, Vec<u8>)> {
        unsafe {
            let mut input = NtfsFileRecordInputBuffer {
                file_reference_number: file_reference_number as i64,
            };
            let header_size = 12usize;
            let mut output = vec![0_u8; header_size + record_size + 16];
            let mut bytes_returned: u32 = 0;
            let success = DeviceIoControl(
                handle,
                FSCTL_GET_NTFS_FILE_RECORD,
                &mut input as *mut _ as *mut c_void,
                std::mem::size_of::<NtfsFileRecordInputBuffer>() as u32,
                output.as_mut_ptr() as *mut c_void,
                output.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            );

            if success == 0 {
                let error = GetLastError();
                anyhow::bail!(
                    "无法读取文件记录 {}（错误码 {}）",
                    file_reference_number,
                    error
                );
            }

            if bytes_returned as usize <= header_size || output.len() < header_size {
                anyhow::bail!("文件记录 {} 返回数据过短", file_reference_number);
            }

            let returned_reference =
                read_i64(&output, 0).unwrap_or(file_reference_number as i64) as u64;
            let file_record_length = read_u32(&output, 8).unwrap_or(0) as usize;
            if file_record_length == 0 {
                anyhow::bail!("文件记录 {} 长度为 0", file_reference_number);
            }

            let available = (bytes_returned as usize).saturating_sub(header_size);
            let actual_len = file_record_length.min(available).min(record_size);
            if actual_len < 4 {
                anyhow::bail!("文件记录 {} 内容过短", file_reference_number);
            }

            Ok((
                returned_reference & FILE_REFERENCE_MASK,
                output[header_size..header_size + actual_len].to_vec(),
            ))
        }
    }

    fn parse_mft_record(
        record_number: u64,
        record_data: &[u8],
        bytes_per_sector: usize,
    ) -> Option<MftRecordInfo> {
        if record_data.len() < 48 || read_u32(record_data, 0)? != MFT_RECORD_SIGNATURE {
            return None;
        }

        let mut data = record_data.to_vec();
        let fixup_offset = read_u16(&data, 4)? as usize;
        let fixup_count = read_u16(&data, 6)? as usize;
        apply_update_sequence_array(&mut data, fixup_offset, fixup_count, bytes_per_sector);

        let offset_to_first_attribute = read_u16(&data, 20)? as usize;
        let flags = read_u16(&data, 22)?;
        if flags & MFT_RECORD_IN_USE == 0 {
            return None;
        }

        let size_in_use = read_u32(&data, 24)? as usize;
        let base_file_reference = read_u64(&data, 32).unwrap_or(0) & FILE_REFERENCE_MASK;
        if base_file_reference != 0 {
            // Extension records do not have a complete filename/data view on their own.
            return None;
        }

        let mut info = MftRecordInfo {
            record_number,
            is_directory: flags & MFT_RECORD_IS_DIRECTORY != 0,
            file_name: String::new(),
            file_namespace_score: 0,
            file_size: 0,
            parent_record: ROOT_FILE_REFERENCE,
            modification_time: None,
        };

        let record_limit = size_in_use.min(data.len());
        let mut offset = offset_to_first_attribute;
        while offset + 16 <= record_limit {
            let attribute_type = read_u32(&data, offset)?;
            if attribute_type == 0xFFFF_FFFF {
                break;
            }

            let attribute_length = read_u32(&data, offset + 4)? as usize;
            if attribute_length < 16 || offset + attribute_length > data.len() {
                break;
            }

            let non_resident = data.get(offset + 8).copied().unwrap_or(0) != 0;
            match attribute_type {
                0x10 => parse_standard_information(&data, offset, non_resident, &mut info),
                0x30 => parse_file_name_attribute(&data, offset, non_resident, &mut info),
                0x80 => parse_data_attribute(&data, offset, non_resident, &mut info),
                _ => {}
            }

            offset += attribute_length;
        }

        if info.file_name.is_empty() {
            None
        } else {
            Some(info)
        }
    }

    fn parse_standard_information(
        data: &[u8],
        attribute_offset: usize,
        non_resident: bool,
        info: &mut MftRecordInfo,
    ) {
        if non_resident {
            return;
        }
        let Some((value_offset, value_length)) = resident_value_range(data, attribute_offset)
        else {
            return;
        };
        if value_length < 36 || value_offset + 36 > data.len() {
            return;
        }

        info.modification_time = read_u64(data, value_offset + 8).or(info.modification_time);
        if let Some(file_attributes) = read_u32(data, value_offset + 32) {
            if file_attributes & FILE_ATTRIBUTE_DIRECTORY_FLAG != 0 {
                info.is_directory = true;
            }
        }
    }

    fn parse_file_name_attribute(
        data: &[u8],
        attribute_offset: usize,
        non_resident: bool,
        info: &mut MftRecordInfo,
    ) {
        if non_resident {
            return;
        }
        let Some((value_offset, value_length)) = resident_value_range(data, attribute_offset)
        else {
            return;
        };
        if value_length < 66 || value_offset + 66 > data.len() {
            return;
        }

        let parent_reference =
            read_u64(data, value_offset).unwrap_or(ROOT_FILE_REFERENCE) & FILE_REFERENCE_MASK;
        let filename_modified = read_u64(data, value_offset + 16);
        let real_size = read_u64(data, value_offset + 48).unwrap_or(0);
        let flags = read_u32(data, value_offset + 56).unwrap_or(0);
        let name_length = data[value_offset + 64] as usize;
        let namespace = data[value_offset + 65];
        let name_offset = value_offset + 66;
        let name_bytes = name_length.saturating_mul(2);

        if name_length == 0 || name_offset + name_bytes > data.len() {
            return;
        }

        let utf16_name: Vec<u16> = data[name_offset..name_offset + name_bytes]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        let Ok(name) = String::from_utf16(&utf16_name) else {
            return;
        };

        let score = filename_namespace_score(namespace);
        if score >= info.file_namespace_score {
            info.file_name = name;
            info.file_namespace_score = score;
            info.parent_record = parent_reference;
            info.modification_time = info.modification_time.or(filename_modified);
        }

        if flags & FILE_ATTRIBUTE_DIRECTORY_FLAG != 0 {
            info.is_directory = true;
        }
        if !info.is_directory && real_size > info.file_size {
            info.file_size = real_size;
        }
    }

    fn parse_data_attribute(
        data: &[u8],
        attribute_offset: usize,
        non_resident: bool,
        info: &mut MftRecordInfo,
    ) {
        if info.is_directory {
            return;
        }

        let name_length = data.get(attribute_offset + 9).copied().unwrap_or(0);
        if name_length != 0 {
            // Alternate data streams are not counted as the primary file size.
            return;
        }

        if non_resident {
            if let Some(data_size) = read_u64(data, attribute_offset + 48) {
                info.file_size = data_size;
            }
        } else if let Some((_value_offset, value_length)) =
            resident_value_range(data, attribute_offset)
        {
            info.file_size = value_length as u64;
        }
    }

    fn resident_value_range(data: &[u8], attribute_offset: usize) -> Option<(usize, usize)> {
        let value_length = read_u32(data, attribute_offset + 16)? as usize;
        let value_offset = read_u16(data, attribute_offset + 20)? as usize;
        let absolute_offset = attribute_offset.checked_add(value_offset)?;
        if absolute_offset.checked_add(value_length)? <= data.len() {
            Some((absolute_offset, value_length))
        } else {
            None
        }
    }

    fn filename_namespace_score(namespace: u8) -> u8 {
        match namespace {
            1 | 3 => 4, // Win32 / Win32 + DOS
            0 => 3,     // POSIX
            2 => 1,     // DOS 8.3 name; only use if no better name exists
            _ => 2,
        }
    }

    fn apply_update_sequence_array(
        data: &mut [u8],
        fixup_offset: usize,
        fixup_count: usize,
        bytes_per_sector: usize,
    ) {
        if fixup_count <= 1 || bytes_per_sector == 0 {
            return;
        }
        let fixup_bytes = fixup_count.saturating_mul(2);
        if fixup_offset + fixup_bytes > data.len() {
            return;
        }

        for sector_index in 1..fixup_count {
            let sector_end = sector_index.saturating_mul(bytes_per_sector);
            if sector_end < 2 || sector_end > data.len() {
                return;
            }
            let replacement_offset = fixup_offset + sector_index * 2;
            data[sector_end - 2] = data[replacement_offset];
            data[sector_end - 1] = data[replacement_offset + 1];
        }
    }

    fn build_full_path(
        record_number: u64,
        nodes: &HashMap<u64, MftNode>,
        root: &std::path::Path,
    ) -> Option<PathBuf> {
        let mut names = Vec::new();
        let mut current = record_number;

        for _ in 0..1024 {
            let node = nodes.get(&current)?;
            if current == ROOT_FILE_REFERENCE || current == node.parent_record || node.name == "." {
                break;
            }

            names.push(node.name.clone());
            current = node.parent_record;
        }

        let mut path = root.to_path_buf();
        for name in names.iter().rev() {
            path.push(name);
        }
        Some(path)
    }

    fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            data.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }

    fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            data.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            data.get(offset..offset + 8)?.try_into().ok()?,
        ))
    }

    fn read_i64(data: &[u8], offset: usize) -> Option<i64> {
        Some(i64::from_le_bytes(
            data.get(offset..offset + 8)?.try_into().ok()?,
        ))
    }

    pub fn scan_mft(config: MftScanConfig, sender: Sender<ScanEvent>) -> Result<MftScanResult> {
        let scan_start = Instant::now();
        validate_mft_target(&config.root, config.drive_letter)?;
        let handle = open_ntfs_volume(config.drive_letter)?;

        let scan_result = (|| {
            let volume_data = get_ntfs_volume_data(handle).context("读取 NTFS 卷信息失败")?;
            let record_size = volume_data.bytes_per_file_record_segment as usize;
            let bytes_per_sector = volume_data.bytes_per_sector as usize;
            let mft_valid_data_length = volume_data.mft_valid_data_length.max(0) as u64;
            let estimated_records = mft_valid_data_length / record_size as u64;

            let mut nodes = HashMap::new();
            let mut last_progress_at = Instant::now();
            for record_index in 0..estimated_records {
                if config.cancel_flag.load(Ordering::Relaxed) {
                    break;
                }

                if let Ok((returned_record_number, record_data)) =
                    get_ntfs_file_record(handle, record_index, record_size)
                {
                    if let Some(record) =
                        parse_mft_record(returned_record_number, &record_data, bytes_per_sector)
                    {
                        nodes.insert(
                            record.record_number,
                            MftNode {
                                parent_record: record.parent_record,
                                name: record.file_name,
                                is_directory: record.is_directory,
                                size: record.file_size,
                                modified: record.modification_time.and_then(filetime_to_systemtime),
                            },
                        );
                    }
                }

                if record_index % PROGRESS_RECORD_INTERVAL == 0
                    && last_progress_at.elapsed() >= PROGRESS_TIME_INTERVAL
                {
                    last_progress_at = Instant::now();
                    let mut stats = ScanStats::default();
                    stats.root = config.root.clone();
                    stats.file_count =
                        nodes.values().filter(|node| !node.is_directory).count() as u64;
                    stats.dir_count =
                        nodes.values().filter(|node| node.is_directory).count() as u64;
                    let _ = sender.send(ScanEvent::Progress(ScanProgress {
                        stats: Arc::new(stats),
                        current_path: Some(PathBuf::from(format!(
                            "MFT 记录 {}/{}",
                            record_index, estimated_records
                        ))),
                        finished: false,
                        cancelled: false,
                        estimated_total_dirs: None,
                        estimated_total_files: Some(estimated_records),
                        scan_mode: crate::scanner::ScanMode::MftScan,
                        active_threads: Some(1),
                    }));
                }
            }

            let mut directories = Vec::new();
            let mut files = Vec::new();
            for (record_number, node) in &nodes {
                let Some(path) = build_full_path(*record_number, &nodes, &config.root) else {
                    continue;
                };

                if node.is_directory {
                    directories.push(path);
                } else {
                    let extension = file_extension_label(&path);
                    files.push(FileRecord {
                        path,
                        size: node.size,
                        modified: node.modified,
                        extension,
                    });
                }
            }

            directories.sort();
            files.sort_by(|left, right| left.path.cmp(&right.path));

            Ok(MftScanResult {
                directories,
                files,
                elapsed: scan_start.elapsed(),
            })
        })();

        unsafe {
            CloseHandle(handle);
        }

        scan_result
    }
}

#[cfg(not(windows))]
pub mod windows_mft {
    use anyhow::Result;

    pub fn scan_mft() -> Result<()> {
        anyhow::bail!("MFT scanning is only supported on Windows");
    }
}
