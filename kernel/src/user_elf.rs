use alloc::vec::Vec;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LE: u8 = 1;
const ELF_VERSION_CURRENT_U8: u8 = 1;
const ELF_VERSION_CURRENT_U32: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;
const ELF64_HEADER_BYTES: usize = 64;
const ELF64_PROGRAM_HEADER_BYTES: usize = 56;
const MAX_LOAD_SEGMENTS: usize = 16;
const DEFAULT_LOAD_OFFSET: usize = 0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserElfError {
    ImageTooSmall,
    BadMagic,
    UnsupportedClass,
    UnsupportedEndian,
    UnsupportedVersion,
    UnsupportedType,
    UnsupportedMachine,
    ProgramHeaderTableOutOfBounds,
    UnsupportedProgramHeaderSize,
    TooManyLoadSegments,
    MissingLoadSegment,
    InvalidLoadSegment,
    WritableExecutableSegment,
    EntryNotExecutable,
    AddressOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserElfSegment<'a> {
    pub virtual_address: u64,
    pub data: &'a [u8],
    pub memory_size: u64,
    pub writable: bool,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserElfImage<'a> {
    pub entry: u64,
    pub segments: Vec<UserElfSegment<'a>>,
}

pub fn parse(bytes: &[u8]) -> Result<UserElfImage<'_>, UserElfError> {
    if bytes.len() < ELF64_HEADER_BYTES {
        return Err(UserElfError::ImageTooSmall);
    }
    if &bytes[0..4] != ELF_MAGIC {
        return Err(UserElfError::BadMagic);
    }
    if bytes[4] != ELF_CLASS_64 {
        return Err(UserElfError::UnsupportedClass);
    }
    if bytes[5] != ELF_DATA_LE {
        return Err(UserElfError::UnsupportedEndian);
    }
    if bytes[6] != ELF_VERSION_CURRENT_U8 || read_u32(bytes, 20)? != ELF_VERSION_CURRENT_U32 {
        return Err(UserElfError::UnsupportedVersion);
    }
    if read_u16(bytes, 16)? != ET_EXEC {
        return Err(UserElfError::UnsupportedType);
    }
    if read_u16(bytes, 18)? != EM_X86_64 {
        return Err(UserElfError::UnsupportedMachine);
    }
    if read_u16(bytes, 52)? as usize != ELF64_HEADER_BYTES {
        return Err(UserElfError::ImageTooSmall);
    }
    if read_u16(bytes, 54)? as usize != ELF64_PROGRAM_HEADER_BYTES {
        return Err(UserElfError::UnsupportedProgramHeaderSize);
    }

    let entry = read_u64(bytes, 24)?;
    let phoff = usize::try_from(read_u64(bytes, 32)?)
        .map_err(|_| UserElfError::ProgramHeaderTableOutOfBounds)?;
    let phnum = read_u16(bytes, 56)? as usize;
    let ph_table_bytes = phnum
        .checked_mul(ELF64_PROGRAM_HEADER_BYTES)
        .ok_or(UserElfError::ProgramHeaderTableOutOfBounds)?;
    let ph_end = phoff
        .checked_add(ph_table_bytes)
        .ok_or(UserElfError::ProgramHeaderTableOutOfBounds)?;
    if phnum == 0 || ph_end > bytes.len() {
        return Err(UserElfError::ProgramHeaderTableOutOfBounds);
    }

    let mut segments = Vec::new();
    for index in 0..phnum {
        let offset = phoff + index * ELF64_PROGRAM_HEADER_BYTES;
        if read_u32(bytes, offset)? != PT_LOAD {
            continue;
        }
        if segments.len() >= MAX_LOAD_SEGMENTS {
            return Err(UserElfError::TooManyLoadSegments);
        }
        let flags = read_u32(bytes, offset + 4)?;
        let file_offset = read_u64(bytes, offset + 8)?;
        let virtual_address = read_u64(bytes, offset + 16)?;
        let file_size = read_u64(bytes, offset + 32)?;
        let memory_size = read_u64(bytes, offset + 40)?;
        let alignment = read_u64(bytes, offset + 48)?;

        if memory_size == 0
            || file_size > memory_size
            || (flags & (PF_W | PF_X)) == (PF_W | PF_X)
            || !valid_alignment(file_offset, virtual_address, alignment)
        {
            if (flags & (PF_W | PF_X)) == (PF_W | PF_X) {
                return Err(UserElfError::WritableExecutableSegment);
            }
            return Err(UserElfError::InvalidLoadSegment);
        }

        let file_start =
            usize::try_from(file_offset).map_err(|_| UserElfError::InvalidLoadSegment)?;
        let file_len = usize::try_from(file_size).map_err(|_| UserElfError::InvalidLoadSegment)?;
        let file_end = file_start
            .checked_add(file_len)
            .ok_or(UserElfError::InvalidLoadSegment)?;
        if file_end > bytes.len() {
            return Err(UserElfError::InvalidLoadSegment);
        }
        virtual_address
            .checked_add(memory_size)
            .ok_or(UserElfError::AddressOverflow)?;
        segments.push(UserElfSegment {
            virtual_address,
            data: &bytes[file_start..file_end],
            memory_size,
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        });
    }

    if segments.is_empty() {
        return Err(UserElfError::MissingLoadSegment);
    }
    if !segments.iter().any(|segment| {
        segment.executable
            && entry >= segment.virtual_address
            && entry
                < segment
                    .virtual_address
                    .saturating_add(segment.data.len() as u64)
    }) {
        return Err(UserElfError::EntryNotExecutable);
    }

    Ok(UserElfImage { entry, segments })
}

pub fn build_flat_rx_executable(load_base: u64, code: &[u8]) -> Result<Vec<u8>, UserElfError> {
    if code.is_empty() {
        return Err(UserElfError::InvalidLoadSegment);
    }
    let memory_size = u64::try_from(code.len()).map_err(|_| UserElfError::AddressOverflow)?;
    load_base
        .checked_add(memory_size)
        .ok_or(UserElfError::AddressOverflow)?;

    let image_len = DEFAULT_LOAD_OFFSET
        .checked_add(code.len())
        .ok_or(UserElfError::AddressOverflow)?;
    let mut image = alloc::vec![0_u8; image_len];
    image[0..4].copy_from_slice(ELF_MAGIC);
    image[4] = ELF_CLASS_64;
    image[5] = ELF_DATA_LE;
    image[6] = ELF_VERSION_CURRENT_U8;
    put_u16(&mut image, 16, ET_EXEC);
    put_u16(&mut image, 18, EM_X86_64);
    put_u32(&mut image, 20, ELF_VERSION_CURRENT_U32);
    put_u64(&mut image, 24, load_base);
    put_u64(&mut image, 32, ELF64_HEADER_BYTES as u64);
    put_u16(&mut image, 52, ELF64_HEADER_BYTES as u16);
    put_u16(&mut image, 54, ELF64_PROGRAM_HEADER_BYTES as u16);
    put_u16(&mut image, 56, 1);

    let ph = ELF64_HEADER_BYTES;
    put_u32(&mut image, ph, PT_LOAD);
    put_u32(&mut image, ph + 4, PF_R | PF_X);
    put_u64(&mut image, ph + 8, DEFAULT_LOAD_OFFSET as u64);
    put_u64(&mut image, ph + 16, load_base);
    put_u64(&mut image, ph + 24, load_base);
    put_u64(&mut image, ph + 32, memory_size);
    put_u64(&mut image, ph + 40, memory_size);
    put_u64(&mut image, ph + 48, 0x1000);
    image[DEFAULT_LOAD_OFFSET..].copy_from_slice(code);

    Ok(image)
}

fn valid_alignment(file_offset: u64, virtual_address: u64, alignment: u64) -> bool {
    if alignment <= 1 {
        return true;
    }
    alignment.is_power_of_two() && file_offset % alignment == virtual_address % alignment
}

fn read_u16(input: &[u8], offset: usize) -> Result<u16, UserElfError> {
    let bytes = input
        .get(offset..offset + 2)
        .ok_or(UserElfError::ImageTooSmall)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, UserElfError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or(UserElfError::ImageTooSmall)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64, UserElfError> {
    let bytes = input
        .get(offset..offset + 8)
        .ok_or(UserElfError::ImageTooSmall)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_segment_executable() {
        let image = build_flat_rx_executable(0x2000_0000_0000, &[0xcc, 0xc3]).unwrap();
        let parsed = parse(&image).unwrap();
        assert_eq!(parsed.entry, 0x2000_0000_0000);
        assert_eq!(parsed.segments.len(), 1);
        assert_eq!(parsed.segments[0].data, &[0xcc, 0xc3]);
        assert!(parsed.segments[0].executable);
        assert!(!parsed.segments[0].writable);
    }

    #[test]
    fn rejects_segments_that_are_writable_and_executable() {
        let mut image = build_flat_rx_executable(0x2000_0000_0000, &[0x90]).unwrap();
        put_u32(&mut image, ELF64_HEADER_BYTES + 4, PF_R | PF_W | PF_X);
        assert_eq!(parse(&image), Err(UserElfError::WritableExecutableSegment));
    }

    #[test]
    fn rejects_entry_outside_executable_bytes() {
        let mut image = build_flat_rx_executable(0x2000_0000_0000, &[0x90]).unwrap();
        put_u64(&mut image, 24, 0x2000_0000_0100);
        assert_eq!(parse(&image), Err(UserElfError::EntryNotExecutable));
    }
}
