use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};

const SUPERBLOCK_MAGIC: &[u8; 8] = b"CDXFS1\0\0";
const RECORD_MAGIC: &[u8; 8] = b"CDXREC1\0";
const FORMAT_VERSION: u32 = 3;
const MIN_FORMAT_VERSION: u32 = 1;
const SUPERBLOCK_A_SECTOR: u64 = 0;
const SUPERBLOCK_B_SECTOR: u64 = 1;
const RECORD_AREA_START: u64 = 8;
const RECORD_SECTORS: u32 = 64;
const RECORD_BYTES: usize = RECORD_SECTORS as usize * SECTOR_SIZE;
const MAX_RECORD_SECTORS: u32 = 8192;
const MAX_RECORD_BYTES: usize = MAX_RECORD_SECTORS as usize * SECTOR_SIZE;
const RECORD_HEADER_BYTES: usize = 64;
const LEGACY_RECORD_ENTRY_BYTES: usize = 8;
const RECORD_ENTRY_BYTES: usize = 12;
const MAX_FILES: usize = 128;
const MAX_PATH_BYTES: usize = 128;
const SUPER_CRC_OFFSET: usize = 44;
const RECORD_HEADER_CRC_OFFSET: usize = 36;
const ENTRY_KIND_FILE: u16 = 1;
const ENTRY_KIND_DIRECTORY: u16 = 2;
pub const DEFAULT_FILE_PERMISSIONS: u16 = 0o644;
pub const DEFAULT_DIRECTORY_PERMISSIONS: u16 = 0o755;
const OWNER_READ: u16 = 0o400;
const OWNER_WRITE: u16 = 0o200;
const OWNER_EXECUTE: u16 = 0o100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountState {
    Formatted,
    Existing,
}

impl MountState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Formatted => "formatted",
            Self::Existing => "existing",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FilesystemInfo {
    pub generation: u64,
    pub file_count: usize,
    pub directory_count: usize,
    pub capacity_sectors: u64,
    pub record_slots: u64,
    pub record_slot_sectors: u32,
    pub active_record_start: u64,
    pub active_record_sectors: u32,
    pub max_record_bytes: usize,
    pub mount_state: MountState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

impl EntryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub path: String,
    pub kind: EntryKind,
    pub permissions: u16,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub kind: EntryKind,
    pub permissions: u16,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    Block(BlockError),
    FlushUnsupported,
    DeviceTooSmall,
    CorruptSuperblocks,
    CorruptRecord,
    InvalidPath,
    TooManyFiles,
    SnapshotTooLarge,
    ArithmeticOverflow,
    InvalidUtf8,
    VerificationFailed,
    NotFound,
    ParentMissing,
    NotDirectory,
    IsDirectory,
    PermissionDenied,
    DirectoryNotEmpty,
    InvalidPermissions,
}

impl From<BlockError> for FsError {
    fn from(error: BlockError) -> Self {
        Self::Block(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileEntry {
    name: String,
    data: Vec<u8>,
    kind: EntryKind,
    permissions: u16,
}

#[derive(Clone, Copy)]
struct Superblock {
    generation: u64,
    record_start: u64,
    record_sectors: u32,
    record_bytes: usize,
    record_crc: u32,
    capacity_sectors: u64,
}

struct LoadedSnapshot {
    superblock: Superblock,
    files: Vec<FileEntry>,
}

pub struct CodexFs<D: BlockDevice> {
    device: D,
    files: Vec<FileEntry>,
    generation: u64,
    record_slots: u64,
    record_slot_sectors: u32,
    active_record_start: u64,
    active_record_sectors: u32,
    mount_state: MountState,
}

impl<D: BlockDevice> CodexFs<D> {
    pub fn mount_or_format(mut device: D) -> Result<Self, FsError> {
        if !device.supports_flush() {
            return Err(FsError::FlushUnsupported);
        }
        let record_slot_sectors = record_slot_span(device.sector_count())?;
        let record_slots = record_slot_count(device.sector_count(), record_slot_sectors)?;
        match load_latest_snapshot(&mut device)? {
            Some(snapshot) => Ok(Self {
                device,
                files: snapshot.files,
                generation: snapshot.superblock.generation,
                record_slots,
                record_slot_sectors,
                active_record_start: snapshot.superblock.record_start,
                active_record_sectors: snapshot.superblock.record_sectors,
                mount_state: MountState::Existing,
            }),
            None => {
                let mut filesystem = Self {
                    device,
                    files: Vec::new(),
                    generation: 0,
                    record_slots,
                    record_slot_sectors,
                    active_record_start: 0,
                    active_record_sectors: 0,
                    mount_state: MountState::Formatted,
                };
                filesystem.commit_files(&[])?;
                Ok(filesystem)
            }
        }
    }

    pub fn info(&self) -> FilesystemInfo {
        FilesystemInfo {
            generation: self.generation,
            file_count: self
                .files
                .iter()
                .filter(|entry| entry.kind == EntryKind::File)
                .count(),
            directory_count: self
                .files
                .iter()
                .filter(|entry| entry.kind == EntryKind::Directory)
                .count(),
            capacity_sectors: self.device.sector_count(),
            record_slots: self.record_slots,
            record_slot_sectors: self.record_slot_sectors,
            active_record_start: self.active_record_start,
            active_record_sectors: self.active_record_sectors,
            max_record_bytes: self.record_slot_sectors as usize * SECTOR_SIZE,
            mount_state: self.mount_state,
        }
    }

    pub fn read_file(&self, path: &str) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|entry| {
                entry.name == path
                    && entry.kind == EntryKind::File
                    && entry.permissions & OWNER_READ != 0
            })
            .map(|entry| entry.data.as_slice())
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), FsError> {
        self.write_file_inner(path, data, None)
    }

    pub fn write_file_with_permissions(
        &mut self,
        path: &str,
        data: &[u8],
        permissions: u16,
    ) -> Result<(), FsError> {
        validate_permissions(permissions)?;
        self.write_file_inner(path, data, Some(permissions))
    }

    pub fn create_dir(&mut self, path: &str, permissions: u16) -> Result<(), FsError> {
        validate_path(path)?;
        validate_permissions(permissions)?;
        let mut next = self.files.clone();
        insert_directory(&mut next, path, permissions)?;
        materialize_missing_parent_directories(&mut next)?;
        if next != self.files {
            self.commit_files(&next)?;
            self.files = next;
        }
        Ok(())
    }

    pub fn create_dir_all(&mut self, path: &str, permissions: u16) -> Result<(), FsError> {
        validate_path(path)?;
        validate_permissions(permissions)?;
        let mut next = self.files.clone();
        let mut current = String::new();
        for component in path.split('/').skip(1) {
            current.push('/');
            current.push_str(component);
            let mode = if current == path {
                permissions
            } else {
                DEFAULT_DIRECTORY_PERMISSIONS
            };
            if let Some(existing) = next.iter().find(|entry| entry.name == current) {
                if existing.kind != EntryKind::Directory {
                    return Err(FsError::NotDirectory);
                }
                continue;
            }
            insert_directory(&mut next, &current, mode)?;
        }
        materialize_missing_parent_directories(&mut next)?;
        if next != self.files {
            self.commit_files(&next)?;
            self.files = next;
        }
        Ok(())
    }

    pub fn metadata(&self, path: &str) -> Result<Metadata, FsError> {
        if path == "/" {
            return Ok(Metadata {
                path: String::from("/"),
                kind: EntryKind::Directory,
                permissions: DEFAULT_DIRECTORY_PERMISSIONS,
                length: 0,
            });
        }
        validate_path(path)?;
        let entry = self
            .files
            .iter()
            .find(|entry| entry.name == path)
            .ok_or(FsError::NotFound)?;
        Ok(Metadata {
            path: entry.name.clone(),
            kind: entry.kind,
            permissions: entry.permissions,
            length: entry.data.len(),
        })
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<DirectoryEntry>, FsError> {
        if path != "/" {
            validate_path(path)?;
            let entry = self
                .files
                .iter()
                .find(|entry| entry.name == path)
                .ok_or(FsError::NotFound)?;
            if entry.kind != EntryKind::Directory {
                return Err(FsError::NotDirectory);
            }
        }

        let mut entries = Vec::new();
        for entry in &self.files {
            if parent_path(&entry.name)? == path {
                entries.push(DirectoryEntry {
                    path: entry.name.clone(),
                    name: leaf_name(&entry.name),
                    kind: entry.kind,
                    permissions: entry.permissions,
                    length: entry.data.len(),
                });
            }
        }
        Ok(entries)
    }

    pub fn set_permissions(&mut self, path: &str, permissions: u16) -> Result<(), FsError> {
        validate_path(path)?;
        validate_permissions(permissions)?;
        let mut next = self.files.clone();
        let entry = next
            .iter_mut()
            .find(|entry| entry.name == path)
            .ok_or(FsError::NotFound)?;
        entry.permissions = permissions;
        materialize_missing_parent_directories(&mut next)?;
        if next != self.files {
            self.commit_files(&next)?;
            self.files = next;
        }
        Ok(())
    }

    pub fn remove_file(&mut self, path: &str) -> Result<(), FsError> {
        validate_path(path)?;
        let mut next = self.files.clone();
        let index = next
            .iter()
            .position(|entry| entry.name == path)
            .ok_or(FsError::NotFound)?;
        if next[index].kind != EntryKind::File {
            return Err(FsError::IsDirectory);
        }
        if next[index].permissions & OWNER_WRITE == 0 {
            return Err(FsError::PermissionDenied);
        }
        next.remove(index);
        materialize_missing_parent_directories(&mut next)?;
        self.commit_files(&next)?;
        self.files = next;
        Ok(())
    }

    pub fn remove_dir(&mut self, path: &str) -> Result<(), FsError> {
        validate_path(path)?;
        let mut next = self.files.clone();
        let index = next
            .iter()
            .position(|entry| entry.name == path)
            .ok_or(FsError::NotFound)?;
        if next[index].kind != EntryKind::Directory {
            return Err(FsError::NotDirectory);
        }
        if next[index].permissions & OWNER_WRITE == 0 {
            return Err(FsError::PermissionDenied);
        }
        for entry in &next {
            if parent_path(&entry.name)? == path {
                return Err(FsError::DirectoryNotEmpty);
            }
        }
        next.remove(index);
        materialize_missing_parent_directories(&mut next)?;
        self.commit_files(&next)?;
        self.files = next;
        Ok(())
    }

    fn write_file_inner(
        &mut self,
        path: &str,
        data: &[u8],
        permissions: Option<u16>,
    ) -> Result<(), FsError> {
        validate_path(path)?;
        ensure_parent_directory(&self.files, path)?;
        let mut next = self.files.clone();
        if let Some(existing) = next.iter_mut().find(|entry| entry.name == path) {
            if existing.kind != EntryKind::File {
                return Err(FsError::IsDirectory);
            }
            if existing.permissions & OWNER_WRITE == 0 {
                return Err(FsError::PermissionDenied);
            }
            existing.data.clear();
            existing.data.extend_from_slice(data);
            if let Some(permissions) = permissions {
                existing.permissions = permissions;
            }
        } else {
            if next.len() >= MAX_FILES {
                return Err(FsError::TooManyFiles);
            }
            next.push(FileEntry {
                name: String::from(path),
                data: data.to_vec(),
                kind: EntryKind::File,
                permissions: permissions.unwrap_or(DEFAULT_FILE_PERMISSIONS),
            });
            next.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        }
        materialize_missing_parent_directories(&mut next)?;
        self.commit_files(&next)?;
        self.files = next;
        Ok(())
    }

    pub fn verify_committed_state(&mut self) -> Result<(), FsError> {
        let loaded = load_latest_snapshot(&mut self.device)?.ok_or(FsError::VerificationFailed)?;
        if loaded.superblock.generation != self.generation || loaded.files != self.files {
            return Err(FsError::VerificationFailed);
        }
        self.active_record_start = loaded.superblock.record_start;
        self.active_record_sectors = loaded.superblock.record_sectors;
        Ok(())
    }

    pub fn unmount(mut self) -> Result<D, FsError> {
        self.device.flush()?;
        Ok(self.device)
    }

    fn commit_files(&mut self, files: &[FileEntry]) -> Result<(), FsError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(FsError::ArithmeticOverflow)?;
        let record = encode_record(generation, files)?;
        let record_sectors =
            u32::try_from(record.len() / SECTOR_SIZE).map_err(|_| FsError::ArithmeticOverflow)?;
        if record_sectors == 0 || record_sectors > self.record_slot_sectors {
            return Err(FsError::SnapshotTooLarge);
        }
        let record_start = self.select_record_start(generation, record_sectors)?;
        let record_crc = crc32(&record);

        for sector_offset in 0..record_sectors {
            let mut sector = [0_u8; SECTOR_SIZE];
            let byte_offset = sector_offset as usize * SECTOR_SIZE;
            sector.copy_from_slice(&record[byte_offset..byte_offset + SECTOR_SIZE]);
            self.device
                .write_sector(record_start + u64::from(sector_offset), &sector)?;
        }
        self.device.flush()?;

        let superblock = Superblock {
            generation,
            record_start,
            record_sectors,
            record_bytes: record.len(),
            record_crc,
            capacity_sectors: self.device.sector_count(),
        };
        let sector = encode_superblock(superblock);
        let super_sector = if generation & 1 == 0 {
            SUPERBLOCK_A_SECTOR
        } else {
            SUPERBLOCK_B_SECTOR
        };
        self.device.write_sector(super_sector, &sector)?;
        self.device.flush()?;
        self.generation = generation;
        self.active_record_start = record_start;
        self.active_record_sectors = record_sectors;
        Ok(())
    }

    fn select_record_start(&self, generation: u64, record_sectors: u32) -> Result<u64, FsError> {
        let slot_span = u64::from(self.record_slot_sectors);
        for attempt in 0..self.record_slots {
            let slot = generation
                .checked_add(attempt)
                .ok_or(FsError::ArithmeticOverflow)?
                % self.record_slots;
            let record_start = RECORD_AREA_START
                .checked_add(
                    slot.checked_mul(slot_span)
                        .ok_or(FsError::ArithmeticOverflow)?,
                )
                .ok_or(FsError::ArithmeticOverflow)?;
            let record_end = record_start
                .checked_add(u64::from(record_sectors))
                .ok_or(FsError::ArithmeticOverflow)?;
            if record_end > self.device.sector_count() {
                continue;
            }
            if self.active_record_sectors != 0
                && ranges_overlap(
                    record_start,
                    record_sectors,
                    self.active_record_start,
                    self.active_record_sectors,
                )?
            {
                continue;
            }
            return Ok(record_start);
        }
        Err(FsError::SnapshotTooLarge)
    }
}

fn load_latest_snapshot<D: BlockDevice>(device: &mut D) -> Result<Option<LoadedSnapshot>, FsError> {
    let mut candidates: [Option<Superblock>; 2] = [None, None];
    let mut saw_nonzero = false;
    for (index, sector_number) in [SUPERBLOCK_A_SECTOR, SUPERBLOCK_B_SECTOR]
        .into_iter()
        .enumerate()
    {
        let mut sector = [0_u8; SECTOR_SIZE];
        device.read_sector(sector_number, &mut sector)?;
        saw_nonzero |= sector.iter().any(|byte| *byte != 0);
        candidates[index] = decode_superblock(&sector, device.sector_count()).ok();
    }

    let mut best: Option<LoadedSnapshot> = None;
    for superblock in candidates.into_iter().flatten() {
        let Ok(files) = read_record(device, superblock) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|current| superblock.generation > current.superblock.generation)
        {
            best = Some(LoadedSnapshot { superblock, files });
        }
    }
    if best.is_none() && saw_nonzero {
        return Err(FsError::CorruptSuperblocks);
    }
    Ok(best)
}

fn read_record<D: BlockDevice>(
    device: &mut D,
    superblock: Superblock,
) -> Result<Vec<FileEntry>, FsError> {
    let mut record = vec![0_u8; superblock.record_bytes];
    for sector_offset in 0..superblock.record_sectors {
        let mut sector = [0_u8; SECTOR_SIZE];
        device.read_sector(
            superblock.record_start + u64::from(sector_offset),
            &mut sector,
        )?;
        let byte_offset = sector_offset as usize * SECTOR_SIZE;
        record[byte_offset..byte_offset + SECTOR_SIZE].copy_from_slice(&sector);
    }
    if crc32(&record) != superblock.record_crc {
        return Err(FsError::CorruptRecord);
    }
    decode_record(&record, superblock.generation)
}

fn encode_superblock(superblock: Superblock) -> [u8; SECTOR_SIZE] {
    let mut sector = [0_u8; SECTOR_SIZE];
    sector[0..8].copy_from_slice(SUPERBLOCK_MAGIC);
    put_u32(&mut sector, 8, FORMAT_VERSION);
    put_u32(&mut sector, 12, 64);
    put_u64(&mut sector, 16, superblock.generation);
    put_u64(&mut sector, 24, superblock.record_start);
    put_u32(&mut sector, 32, superblock.record_sectors);
    put_u32(&mut sector, 36, superblock.record_bytes as u32);
    put_u32(&mut sector, 40, superblock.record_crc);
    put_u64(&mut sector, 48, superblock.capacity_sectors);
    let checksum = crc32(&sector);
    put_u32(&mut sector, SUPER_CRC_OFFSET, checksum);
    sector
}

fn decode_superblock(
    sector: &[u8; SECTOR_SIZE],
    actual_capacity: u64,
) -> Result<Superblock, FsError> {
    let version = get_u32(sector, 8)?;
    if &sector[0..8] != SUPERBLOCK_MAGIC
        || !(MIN_FORMAT_VERSION..=FORMAT_VERSION).contains(&version)
        || get_u32(sector, 12)? != 64
    {
        return Err(FsError::CorruptSuperblocks);
    }
    let record_sectors = get_u32(sector, 32)?;
    let record_bytes = get_u32(sector, 36)? as usize;
    if !valid_record_layout(version, record_sectors, record_bytes) {
        return Err(FsError::CorruptSuperblocks);
    }
    let stored_checksum = get_u32(sector, SUPER_CRC_OFFSET)?;
    let mut copy = *sector;
    put_u32(&mut copy, SUPER_CRC_OFFSET, 0);
    if crc32(&copy) != stored_checksum {
        return Err(FsError::CorruptSuperblocks);
    }
    let generation = get_u64(sector, 16)?;
    let record_start = get_u64(sector, 24)?;
    let capacity_sectors = get_u64(sector, 48)?;
    let record_end = record_start
        .checked_add(u64::from(record_sectors))
        .ok_or(FsError::ArithmeticOverflow)?;
    if generation == 0 || record_start < RECORD_AREA_START || record_end > actual_capacity {
        return Err(FsError::CorruptSuperblocks);
    }
    if version < 3 && !(record_start - RECORD_AREA_START).is_multiple_of(u64::from(RECORD_SECTORS))
    {
        return Err(FsError::CorruptSuperblocks);
    }
    if capacity_sectors > actual_capacity {
        return Err(FsError::CorruptSuperblocks);
    }
    Ok(Superblock {
        generation,
        record_start,
        record_sectors,
        record_bytes,
        record_crc: get_u32(sector, 40)?,
        capacity_sectors,
    })
}

fn encode_record(generation: u64, files: &[FileEntry]) -> Result<Vec<u8>, FsError> {
    if files.len() > MAX_FILES {
        return Err(FsError::TooManyFiles);
    }
    validate_tree(files, true)?;
    let required_bytes = required_record_bytes(files)?;
    let record_bytes = align_up_usize(required_bytes, SECTOR_SIZE)?.max(RECORD_BYTES);
    if record_bytes > MAX_RECORD_BYTES {
        return Err(FsError::SnapshotTooLarge);
    }
    let mut record = vec![0_u8; record_bytes];
    record[0..8].copy_from_slice(RECORD_MAGIC);
    put_u32(&mut record, 8, FORMAT_VERSION);
    put_u32(&mut record, 12, RECORD_HEADER_BYTES as u32);
    put_u64(&mut record, 16, generation);
    put_u32(&mut record, 24, files.len() as u32);

    let mut cursor = RECORD_HEADER_BYTES;
    for file in files {
        validate_path(&file.name)?;
        let name = file.name.as_bytes();
        put_u16(&mut record, cursor, name.len() as u16);
        put_u16(&mut record, cursor + 2, encode_kind(file.kind));
        put_u32(&mut record, cursor + 4, file.data.len() as u32);
        put_u16(&mut record, cursor + 8, file.permissions);
        put_u16(&mut record, cursor + 10, 0);
        cursor += RECORD_ENTRY_BYTES;
        record[cursor..cursor + name.len()].copy_from_slice(name);
        cursor += name.len();
        record[cursor..cursor + file.data.len()].copy_from_slice(&file.data);
        cursor += file.data.len();
    }
    let payload_bytes = cursor - RECORD_HEADER_BYTES;
    put_u32(&mut record, 28, payload_bytes as u32);
    let payload_checksum = crc32(&record[RECORD_HEADER_BYTES..cursor]);
    put_u32(&mut record, 32, payload_checksum);
    let header_checksum = crc32(&record[..RECORD_HEADER_BYTES]);
    put_u32(&mut record, RECORD_HEADER_CRC_OFFSET, header_checksum);
    Ok(record)
}

fn decode_record(record: &[u8], expected_generation: u64) -> Result<Vec<FileEntry>, FsError> {
    if record.len() < RECORD_HEADER_BYTES {
        return Err(FsError::CorruptRecord);
    }
    let version = get_u32(record, 8)?;
    if &record[0..8] != RECORD_MAGIC
        || !(MIN_FORMAT_VERSION..=FORMAT_VERSION).contains(&version)
        || get_u32(record, 12)? as usize != RECORD_HEADER_BYTES
        || get_u64(record, 16)? != expected_generation
    {
        return Err(FsError::CorruptRecord);
    }
    let mut header = [0_u8; RECORD_HEADER_BYTES];
    header.copy_from_slice(&record[..RECORD_HEADER_BYTES]);
    let stored_header_crc = get_u32(&header, RECORD_HEADER_CRC_OFFSET)?;
    put_u32(&mut header, RECORD_HEADER_CRC_OFFSET, 0);
    if crc32(&header) != stored_header_crc {
        return Err(FsError::CorruptRecord);
    }
    let entry_count = get_u32(record, 24)? as usize;
    let payload_bytes = get_u32(record, 28)? as usize;
    if entry_count > MAX_FILES {
        return Err(FsError::CorruptRecord);
    }
    let payload_end = RECORD_HEADER_BYTES
        .checked_add(payload_bytes)
        .filter(|end| *end <= record.len())
        .ok_or(FsError::CorruptRecord)?;
    if crc32(&record[RECORD_HEADER_BYTES..payload_end]) != get_u32(record, 32)? {
        return Err(FsError::CorruptRecord);
    }

    let mut files = Vec::with_capacity(entry_count);
    let mut cursor = RECORD_HEADER_BYTES;
    for _ in 0..entry_count {
        let entry_header_bytes = if version == 1 {
            LEGACY_RECORD_ENTRY_BYTES
        } else {
            RECORD_ENTRY_BYTES
        };
        let metadata_end = cursor
            .checked_add(entry_header_bytes)
            .ok_or(FsError::CorruptRecord)?;
        if metadata_end > payload_end {
            return Err(FsError::CorruptRecord);
        }
        let name_len = get_u16(record, cursor)? as usize;
        let flags = get_u16(record, cursor + 2)?;
        let data_len = get_u32(record, cursor + 4)? as usize;
        let (kind, permissions) = if version == 1 {
            if flags != 0 {
                return Err(FsError::CorruptRecord);
            }
            (EntryKind::File, DEFAULT_FILE_PERMISSIONS)
        } else {
            let kind = decode_kind(flags)?;
            let permissions = get_u16(record, cursor + 8)?;
            if get_u16(record, cursor + 10)? != 0 {
                return Err(FsError::CorruptRecord);
            }
            validate_permissions(permissions)?;
            (kind, permissions)
        };
        cursor = metadata_end;
        let name_end = cursor.checked_add(name_len).ok_or(FsError::CorruptRecord)?;
        let data_end = name_end
            .checked_add(data_len)
            .ok_or(FsError::CorruptRecord)?;
        if data_end > payload_end {
            return Err(FsError::CorruptRecord);
        }
        let name =
            core::str::from_utf8(&record[cursor..name_end]).map_err(|_| FsError::InvalidUtf8)?;
        validate_path(name)?;
        if kind == EntryKind::Directory && data_len != 0 {
            return Err(FsError::CorruptRecord);
        }
        if files.iter().any(|entry: &FileEntry| entry.name == name) {
            return Err(FsError::CorruptRecord);
        }
        files.push(FileEntry {
            name: String::from(name),
            data: record[name_end..data_end].to_vec(),
            kind,
            permissions,
        });
        cursor = data_end;
    }
    if cursor != payload_end {
        return Err(FsError::CorruptRecord);
    }
    validate_tree(&files, version != 1)?;
    Ok(files)
}

fn required_record_bytes(files: &[FileEntry]) -> Result<usize, FsError> {
    let mut bytes = RECORD_HEADER_BYTES;
    for file in files {
        bytes = bytes
            .checked_add(RECORD_ENTRY_BYTES)
            .and_then(|value| value.checked_add(file.name.len()))
            .and_then(|value| value.checked_add(file.data.len()))
            .ok_or(FsError::ArithmeticOverflow)?;
    }
    Ok(bytes)
}

fn validate_path(path: &str) -> Result<(), FsError> {
    if path.len() < 2
        || path.len() > MAX_PATH_BYTES
        || !path.starts_with('/')
        || path.ends_with('/')
        || path.bytes().any(|byte| byte == 0 || byte < 0x20)
        || path
            .split('/')
            .skip(1)
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(FsError::InvalidPath);
    }
    Ok(())
}

fn validate_permissions(permissions: u16) -> Result<(), FsError> {
    if permissions & !0o777 != 0 {
        return Err(FsError::InvalidPermissions);
    }
    Ok(())
}

fn materialize_missing_parent_directories(entries: &mut Vec<FileEntry>) -> Result<(), FsError> {
    let mut required = Vec::<String>::new();
    for entry in entries.iter() {
        let mut parent = parent_path(&entry.name)?;
        while parent != "/" {
            if !required.iter().any(|path| path == parent) {
                required.push(String::from(parent));
            }
            parent = parent_path(parent)?;
        }
    }
    required.sort_unstable();
    for path in required {
        if let Some(existing) = entries.iter().find(|entry| entry.name == path) {
            if existing.kind != EntryKind::Directory {
                return Err(FsError::NotDirectory);
            }
            continue;
        }
        if entries.len() >= MAX_FILES {
            return Err(FsError::TooManyFiles);
        }
        entries.push(FileEntry {
            name: path,
            data: Vec::new(),
            kind: EntryKind::Directory,
            permissions: DEFAULT_DIRECTORY_PERMISSIONS,
        });
    }
    entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

fn validate_tree(files: &[FileEntry], require_directories: bool) -> Result<(), FsError> {
    for entry in files {
        validate_path(&entry.name)?;
        validate_permissions(entry.permissions)?;
        if entry.kind == EntryKind::Directory && !entry.data.is_empty() {
            return Err(FsError::CorruptRecord);
        }
        if entry.kind == EntryKind::Directory && entry.permissions & OWNER_EXECUTE == 0 {
            return Err(FsError::PermissionDenied);
        }
        if require_directories {
            ensure_parent_directory(files, &entry.name)?;
        }
    }
    Ok(())
}

fn insert_directory(
    entries: &mut Vec<FileEntry>,
    path: &str,
    permissions: u16,
) -> Result<(), FsError> {
    ensure_parent_directory(entries, path)?;
    if let Some(existing) = entries.iter_mut().find(|entry| entry.name == path) {
        if existing.kind != EntryKind::Directory {
            return Err(FsError::NotDirectory);
        }
        existing.permissions = permissions;
        return Ok(());
    }
    if entries.len() >= MAX_FILES {
        return Err(FsError::TooManyFiles);
    }
    entries.push(FileEntry {
        name: String::from(path),
        data: Vec::new(),
        kind: EntryKind::Directory,
        permissions,
    });
    entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(())
}

fn ensure_parent_directory(entries: &[FileEntry], path: &str) -> Result<(), FsError> {
    let parent = parent_path(path)?;
    if parent == "/" {
        return Ok(());
    }
    let Some(parent) = entries.iter().find(|entry| entry.name == parent) else {
        return Err(FsError::ParentMissing);
    };
    if parent.kind != EntryKind::Directory {
        return Err(FsError::NotDirectory);
    }
    if parent.permissions & OWNER_EXECUTE == 0 || parent.permissions & OWNER_WRITE == 0 {
        return Err(FsError::PermissionDenied);
    }
    Ok(())
}

fn parent_path(path: &str) -> Result<&str, FsError> {
    if path == "/" {
        return Err(FsError::InvalidPath);
    }
    validate_path(path)?;
    let Some(index) = path.rfind('/') else {
        return Err(FsError::InvalidPath);
    };
    if index == 0 {
        Ok("/")
    } else {
        Ok(&path[..index])
    }
}

fn leaf_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .map_or_else(String::new, String::from)
}

fn encode_kind(kind: EntryKind) -> u16 {
    match kind {
        EntryKind::File => ENTRY_KIND_FILE,
        EntryKind::Directory => ENTRY_KIND_DIRECTORY,
    }
}

fn decode_kind(raw: u16) -> Result<EntryKind, FsError> {
    match raw {
        ENTRY_KIND_FILE => Ok(EntryKind::File),
        ENTRY_KIND_DIRECTORY => Ok(EntryKind::Directory),
        _ => Err(FsError::CorruptRecord),
    }
}

fn valid_record_layout(version: u32, record_sectors: u32, record_bytes: usize) -> bool {
    if record_sectors == 0 || record_sectors > MAX_RECORD_SECTORS {
        return false;
    }
    if !(RECORD_HEADER_BYTES..=MAX_RECORD_BYTES).contains(&record_bytes) {
        return false;
    }
    if record_bytes != record_sectors as usize * SECTOR_SIZE {
        return false;
    }
    version >= 3 || (record_sectors == RECORD_SECTORS && record_bytes == RECORD_BYTES)
}

fn record_slot_span(sector_count: u64) -> Result<u32, FsError> {
    let available = sector_count
        .checked_sub(RECORD_AREA_START)
        .ok_or(FsError::DeviceTooSmall)?;
    let half = available / 2;
    if half < u64::from(RECORD_SECTORS) {
        return Err(FsError::DeviceTooSmall);
    }
    let span = core::cmp::min(u64::from(MAX_RECORD_SECTORS), half);
    u32::try_from(span).map_err(|_| FsError::ArithmeticOverflow)
}

fn record_slot_count(sector_count: u64, slot_span_sectors: u32) -> Result<u64, FsError> {
    let available = sector_count
        .checked_sub(RECORD_AREA_START)
        .ok_or(FsError::DeviceTooSmall)?;
    let slots = available / u64::from(slot_span_sectors);
    if slots < 2 {
        return Err(FsError::DeviceTooSmall);
    }
    Ok(slots)
}

fn ranges_overlap(
    left_start: u64,
    left_sectors: u32,
    right_start: u64,
    right_sectors: u32,
) -> Result<bool, FsError> {
    let left_end = left_start
        .checked_add(u64::from(left_sectors))
        .ok_or(FsError::ArithmeticOverflow)?;
    let right_end = right_start
        .checked_add(u64::from(right_sectors))
        .ok_or(FsError::ArithmeticOverflow)?;
    Ok(left_start < right_end && right_start < left_end)
}

fn align_up_usize(value: usize, alignment: usize) -> Result<usize, FsError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(FsError::ArithmeticOverflow)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(input: &[u8], offset: usize) -> Result<u16, FsError> {
    let bytes = input
        .get(offset..offset + 2)
        .ok_or(FsError::CorruptRecord)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn get_u32(input: &[u8], offset: usize) -> Result<u32, FsError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or(FsError::CorruptRecord)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn get_u64(input: &[u8], offset: usize) -> Result<u64, FsError> {
    let bytes = input
        .get(offset..offset + 8)
        .ok_or(FsError::CorruptRecord)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDisk {
        sectors: Vec<[u8; SECTOR_SIZE]>,
        flush_count: usize,
    }

    impl TestDisk {
        fn new(sector_count: usize) -> Self {
            Self {
                sectors: vec![[0_u8; SECTOR_SIZE]; sector_count],
                flush_count: 0,
            }
        }
    }

    impl BlockDevice for TestDisk {
        fn sector_count(&self) -> u64 {
            self.sectors.len() as u64
        }

        fn supports_flush(&self) -> bool {
            true
        }

        fn read_sector(
            &mut self,
            sector: u64,
            output: &mut [u8; SECTOR_SIZE],
        ) -> Result<(), BlockError> {
            *output = *self
                .sectors
                .get(sector as usize)
                .ok_or(BlockError::SectorOutOfRange)?;
            Ok(())
        }

        fn write_sector(
            &mut self,
            sector: u64,
            input: &[u8; SECTOR_SIZE],
        ) -> Result<(), BlockError> {
            *self
                .sectors
                .get_mut(sector as usize)
                .ok_or(BlockError::SectorOutOfRange)? = *input;
            Ok(())
        }

        fn flush(&mut self) -> Result<(), BlockError> {
            self.flush_count += 1;
            Ok(())
        }
    }

    #[test]
    fn crc_matches_the_standard_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn path_validation_rejects_ambiguous_or_relative_names() {
        assert!(validate_path("/system/boot-count").is_ok());
        assert!(validate_path("relative").is_err());
        assert!(validate_path("/system//count").is_err());
        assert!(validate_path("/system/../count").is_err());
        assert!(validate_path("/system/count/").is_err());
    }

    #[test]
    fn record_round_trip_preserves_sorted_file_data() {
        let files = vec![
            FileEntry {
                name: String::from("/config"),
                data: Vec::new(),
                kind: EntryKind::Directory,
                permissions: DEFAULT_DIRECTORY_PERMISSIONS,
            },
            FileEntry {
                name: String::from("/config/network"),
                data: b"dhcp=true".to_vec(),
                kind: EntryKind::File,
                permissions: DEFAULT_FILE_PERMISSIONS,
            },
            FileEntry {
                name: String::from("/system"),
                data: Vec::new(),
                kind: EntryKind::Directory,
                permissions: DEFAULT_DIRECTORY_PERMISSIONS,
            },
            FileEntry {
                name: String::from("/system/boot-count"),
                data: b"42".to_vec(),
                kind: EntryKind::File,
                permissions: DEFAULT_FILE_PERMISSIONS,
            },
        ];
        let record = encode_record(9, &files).unwrap();
        assert_eq!(decode_record(&record, 9).unwrap(), files);
    }

    #[test]
    fn legacy_records_load_as_regular_files_with_default_permissions() {
        let mut record = vec![0_u8; RECORD_BYTES];
        record[0..8].copy_from_slice(RECORD_MAGIC);
        put_u32(&mut record, 8, 1);
        put_u32(&mut record, 12, RECORD_HEADER_BYTES as u32);
        put_u64(&mut record, 16, 7);
        put_u32(&mut record, 24, 1);

        let mut cursor = RECORD_HEADER_BYTES;
        let name = b"/system/boot-count";
        let data = b"9";
        put_u16(&mut record, cursor, name.len() as u16);
        put_u16(&mut record, cursor + 2, 0);
        put_u32(&mut record, cursor + 4, data.len() as u32);
        cursor += LEGACY_RECORD_ENTRY_BYTES;
        record[cursor..cursor + name.len()].copy_from_slice(name);
        cursor += name.len();
        record[cursor..cursor + data.len()].copy_from_slice(data);
        cursor += data.len();

        let payload_bytes = cursor - RECORD_HEADER_BYTES;
        put_u32(&mut record, 28, payload_bytes as u32);
        let payload_checksum = crc32(&record[RECORD_HEADER_BYTES..cursor]);
        put_u32(&mut record, 32, payload_checksum);
        put_u32(&mut record, RECORD_HEADER_CRC_OFFSET, 0);
        let header_checksum = crc32(&record[..RECORD_HEADER_BYTES]);
        put_u32(&mut record, RECORD_HEADER_CRC_OFFSET, header_checksum);

        let files = decode_record(&record, 7).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "/system/boot-count");
        assert_eq!(files[0].data, b"9");
        assert_eq!(files[0].kind, EntryKind::File);
        assert_eq!(files[0].permissions, DEFAULT_FILE_PERMISSIONS);
    }

    #[test]
    fn record_checksum_detects_payload_corruption() {
        let files = vec![
            FileEntry {
                name: String::from("/system"),
                data: Vec::new(),
                kind: EntryKind::Directory,
                permissions: DEFAULT_DIRECTORY_PERMISSIONS,
            },
            FileEntry {
                name: String::from("/system/value"),
                data: b"durable".to_vec(),
                kind: EntryKind::File,
                permissions: DEFAULT_FILE_PERMISSIONS,
            },
        ];
        let mut record = encode_record(4, &files).unwrap();
        record[RECORD_HEADER_BYTES + 10] ^= 0x80;
        assert_eq!(decode_record(&record, 4), Err(FsError::CorruptRecord));
    }

    #[test]
    fn committed_file_survives_unmount_and_remount() {
        let mut filesystem = CodexFs::mount_or_format(TestDisk::new(300)).unwrap();
        assert_eq!(filesystem.info().mount_state, MountState::Formatted);
        filesystem.create_dir_all("/system", 0o755).unwrap();
        filesystem
            .write_file_with_permissions("/system/boot-count", b"17", 0o600)
            .unwrap();
        filesystem.verify_committed_state().unwrap();
        let disk = filesystem.unmount().unwrap();
        assert!(disk.flush_count >= 5);

        let filesystem = CodexFs::mount_or_format(disk).unwrap();
        assert_eq!(filesystem.info().mount_state, MountState::Existing);
        assert_eq!(filesystem.read_file("/system/boot-count"), Some(&b"17"[..]));
        assert_eq!(
            filesystem.metadata("/system").unwrap().kind,
            EntryKind::Directory
        );
        assert_eq!(
            filesystem
                .metadata("/system/boot-count")
                .unwrap()
                .permissions,
            0o600
        );
    }

    #[test]
    fn large_file_survives_dynamic_multisector_snapshot() {
        let mut filesystem = CodexFs::mount_or_format(TestDisk::new(1024)).unwrap();
        filesystem
            .create_dir_all("/system/packages", 0o755)
            .unwrap();
        let mut package = vec![0_u8; RECORD_BYTES + 17_000];
        for (index, byte) in package.iter_mut().enumerate() {
            *byte = (index.wrapping_mul(31).wrapping_add(index / 251) & 0xff) as u8;
        }

        filesystem
            .write_file_with_permissions("/system/packages/app.pkg", &package, 0o640)
            .unwrap();
        let info = filesystem.info();
        assert!(info.active_record_sectors > RECORD_SECTORS);
        assert!(info.max_record_bytes > RECORD_BYTES);
        assert_eq!(
            filesystem
                .metadata("/system/packages/app.pkg")
                .unwrap()
                .length,
            package.len()
        );
        filesystem.verify_committed_state().unwrap();
        let disk = filesystem.unmount().unwrap();

        let filesystem = CodexFs::mount_or_format(disk).unwrap();
        assert_eq!(
            filesystem.read_file("/system/packages/app.pkg"),
            Some(package.as_slice())
        );
        assert!(filesystem.info().active_record_sectors > RECORD_SECTORS);
    }

    #[test]
    fn dynamic_superblock_round_trip_preserves_record_span() {
        let superblock = Superblock {
            generation: 33,
            record_start: 4096,
            record_sectors: RECORD_SECTORS + 9,
            record_bytes: (RECORD_SECTORS as usize + 9) * SECTOR_SIZE,
            record_crc: 0x1234_5678,
            capacity_sectors: 131_072,
        };
        let encoded = encode_superblock(superblock);
        let decoded = decode_superblock(&encoded, 131_072).unwrap();
        assert_eq!(decoded.generation, superblock.generation);
        assert_eq!(decoded.record_start, superblock.record_start);
        assert_eq!(decoded.record_sectors, superblock.record_sectors);
        assert_eq!(decoded.record_bytes, superblock.record_bytes);
        assert_eq!(decoded.record_crc, superblock.record_crc);
    }

    #[test]
    fn damaged_newest_record_falls_back_to_previous_generation() {
        let mut filesystem = CodexFs::mount_or_format(TestDisk::new(300)).unwrap();
        filesystem.create_dir_all("/system", 0o755).unwrap();
        filesystem.write_file("/system/value", b"stable").unwrap();
        let newest_record_start = filesystem.info().active_record_start;
        let mut disk = filesystem.unmount().unwrap();
        disk.sectors[newest_record_start as usize][RECORD_HEADER_BYTES] ^= 0x80;

        let filesystem = CodexFs::mount_or_format(disk).unwrap();
        assert_eq!(filesystem.info().generation, 2);
        assert_eq!(filesystem.read_file("/system/value"), None);
    }

    #[test]
    fn damaged_superblocks_are_never_silently_reformatted() {
        let filesystem = CodexFs::mount_or_format(TestDisk::new(160)).unwrap();
        let mut disk = filesystem.unmount().unwrap();
        disk.sectors[SUPERBLOCK_B_SECTOR as usize][0] ^= 0x80;
        assert!(matches!(
            CodexFs::mount_or_format(disk),
            Err(FsError::CorruptSuperblocks)
        ));
    }

    #[test]
    fn directories_permissions_listing_and_removal_are_enforced() {
        let mut filesystem = CodexFs::mount_or_format(TestDisk::new(160)).unwrap();
        filesystem.create_dir_all("/system/bin", 0o755).unwrap();
        filesystem
            .write_file_with_permissions("/system/bin/app", b"run", 0o555)
            .unwrap();

        let listing = filesystem.list_dir("/system").unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "bin");
        assert_eq!(listing[0].kind, EntryKind::Directory);
        let metadata = filesystem.metadata("/system/bin/app").unwrap();
        assert_eq!(metadata.kind, EntryKind::File);
        assert_eq!(metadata.permissions, 0o555);
        assert_eq!(
            filesystem.write_file("/system/bin/app", b"new"),
            Err(FsError::PermissionDenied)
        );
        assert_eq!(
            filesystem.remove_dir("/system/bin"),
            Err(FsError::DirectoryNotEmpty)
        );
        filesystem
            .set_permissions("/system/bin/app", 0o755)
            .unwrap();
        filesystem.remove_file("/system/bin/app").unwrap();
        filesystem.remove_dir("/system/bin").unwrap();
    }

    #[test]
    fn file_writes_require_existing_parent_directory() {
        let mut filesystem = CodexFs::mount_or_format(TestDisk::new(160)).unwrap();
        assert_eq!(
            filesystem.write_file("/system/missing", b"value"),
            Err(FsError::ParentMissing)
        );
        filesystem.create_dir_all("/system", 0o755).unwrap();
        filesystem.write_file("/system/missing", b"value").unwrap();
    }

    #[test]
    fn migration_materializes_missing_parent_directories() {
        let mut entries = vec![
            FileEntry {
                name: String::from("/system/boot-count"),
                data: b"1".to_vec(),
                kind: EntryKind::File,
                permissions: DEFAULT_FILE_PERMISSIONS,
            },
            FileEntry {
                name: String::from("/system/bin/app"),
                data: b"app".to_vec(),
                kind: EntryKind::File,
                permissions: DEFAULT_FILE_PERMISSIONS,
            },
        ];
        materialize_missing_parent_directories(&mut entries).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| { entry.name == "/system" && entry.kind == EntryKind::Directory })
        );
        assert!(
            entries
                .iter()
                .any(|entry| { entry.name == "/system/bin" && entry.kind == EntryKind::Directory })
        );
        encode_record(11, &entries).unwrap();
    }
}
