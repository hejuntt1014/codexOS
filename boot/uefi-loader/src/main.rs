#![no_std]
#![no_main]

extern crate alloc;

use bootinfo::{
    BootInfo, FirmwareMode, FrameBufferInfo, KERNEL_SEGMENT_FLAG_EXECUTE,
    KERNEL_SEGMENT_FLAG_READ, KERNEL_SEGMENT_FLAG_WRITE, KernelImageInfo, KernelImageSegment,
    MAX_KERNEL_IMAGE_SEGMENTS, MAX_MEMORY_REGIONS, MAX_RESERVED_MEMORY_RANGES, MemoryRegion,
    MemoryRegionKind, PAGE_SIZE, PixelFormat, ReservedMemoryKind, ReservedMemoryRange,
};
use core::mem;
use core::ptr::{copy_nonoverlapping, write_bytes};
use core::time::Duration;
use kernel::{DesktopApp, DesktopInput, PointerSample, init};
use uefi::boot::{self, AllocateType};
use uefi::fs::FileSystem;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};
use uefi::proto::console::pointer::Pointer;
use uefi::proto::console::text::{Input, Key, ScanCode};
use uefi::proto::loaded_image::LoadedImage;
use uefi::CString16;
use xmas_elf::ElfFile;
use xmas_elf::program::Type as ProgramType;

const LOW_MEMORY_RESERVE_BYTES: u64 = 1024 * 1024;
const RUNTIME_HHDM_BASE: u64 = 0xffff_8000_0000_0000;

#[entry]
fn main() -> Status {
    let handle = match boot::get_handle_for_protocol::<GraphicsOutput>() {
        Ok(handle) => handle,
        Err(status) => return status.status(),
    };

    let mut gop = match boot::open_protocol_exclusive::<GraphicsOutput>(handle) {
        Ok(gop) => gop,
        Err(status) => return status.status(),
    };

    let framebuffer = read_framebuffer_info(&mut gop);
    let loader_image = match boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()) {
        Ok(image) => {
            let (base, size) = image.info();
            Some((base as u64, size))
        }
        Err(status) => return status.status(),
    };
    let kernel_image = load_kernel_image();

    if cfg!(feature = "handoff") {
        return run_handoff_mode(gop, framebuffer, loader_image, kernel_image);
    }

    let memory_map = match boot::memory_map(MemoryType::LOADER_DATA) {
        Ok(memory_map) => memory_map,
        Err(status) => return status.status(),
    };

    let mut boot_info = new_boot_info(framebuffer, FirmwareMode::UefiBootServices, kernel_image);
    collect_memory_regions(&mut boot_info, &memory_map);
    collect_reserved_ranges(
        &mut boot_info,
        loader_image,
        Some((
            memory_map.buffer().as_ptr() as u64,
            memory_map.buffer().len() as u64,
        )),
    );

    init(&boot_info);
    log_boot_context(&boot_info);

    let input_handle = match boot::get_handle_for_protocol::<Input>() {
        Ok(handle) => handle,
        Err(status) => return status.status(),
    };
    let mut input = match boot::open_protocol_exclusive::<Input>(input_handle) {
        Ok(input) => input,
        Err(status) => return status.status(),
    };

    let mut pointer = boot::get_handle_for_protocol::<Pointer>()
        .ok()
        .and_then(|handle| boot::open_protocol_exclusive::<Pointer>(handle).ok());

    let _ = input.reset(false);
    if let Some(pointer) = pointer.as_mut() {
        let _ = pointer.reset(false);
        kernel::log_line(format_args!("pointer input: available"));
    } else {
        kernel::log_line(format_args!("pointer input: unavailable"));
    }

    let mut desktop = DesktopApp::new(&boot_info);

    loop {
        while let Ok(Some(key)) = input.read_key() {
            if let Some(action) = translate_key(key) {
                desktop.handle_input(action);
            }
        }

        if let Some(pointer) = pointer.as_mut() {
            while let Ok(Some(state)) = pointer.read_state() {
                let sample = PointerSample {
                    delta_x: scale_pointer_delta(state.relative_movement[0]),
                    delta_y: scale_pointer_delta(state.relative_movement[1]),
                    left_button: state.button[0],
                    right_button: state.button[1],
                };
                desktop.handle_pointer(sample);
            }
        }

        if desktop.needs_redraw() {
            desktop.render(&boot_info);
        }

        if desktop.should_exit() {
            break;
        }

        boot::stall(Duration::from_millis(16));
    }

    Status::SUCCESS
}

fn run_handoff_mode(
    gop: uefi::boot::ScopedProtocol<GraphicsOutput>,
    framebuffer: FrameBufferInfo,
    loader_image: Option<(u64, u64)>,
    kernel_image: KernelImageInfo,
) -> Status {
    core::mem::forget(gop);

    let memory_map = unsafe { boot::exit_boot_services(None) };
    let mut boot_info = new_boot_info(
        framebuffer,
        FirmwareMode::PostExitBootServices,
        kernel_image,
    );
    collect_memory_regions(&mut boot_info, &memory_map);
    collect_reserved_ranges(
        &mut boot_info,
        loader_image,
        Some((
            memory_map.buffer().as_ptr() as u64,
            memory_map.buffer().len() as u64,
        )),
    );

    init(&boot_info);
    log_boot_context(&boot_info);
    kernel::activate_post_ebs_vm(&boot_info);

    if cfg!(feature = "chainload") {
        return chainload_kernel(&boot_info);
    }

    let mut desktop = DesktopApp::new(&boot_info);
    desktop.note_handoff_complete();
    desktop.render(&boot_info);

    loop {
        core::hint::spin_loop();
    }
}

fn read_framebuffer_info(gop: &mut GraphicsOutput) -> FrameBufferInfo {
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let mut frame_buffer = gop.frame_buffer();

    FrameBufferInfo {
        base: frame_buffer.as_mut_ptr(),
        size: frame_buffer.size(),
        width: width as u32,
        height: height as u32,
        stride: mode.stride(),
        bytes_per_pixel: 4,
        pixel_format: map_pixel_format(mode.pixel_format()),
    }
}

fn new_boot_info(
    framebuffer: FrameBufferInfo,
    firmware_mode: FirmwareMode,
    kernel_image: KernelImageInfo,
) -> BootInfo {
    BootInfo {
        framebuffer,
        firmware_mode,
        runtime_hhdm_base: RUNTIME_HHDM_BASE,
        memory_region_count: 0,
        memory_region_total: 0,
        memory_regions: [MemoryRegion::EMPTY; MAX_MEMORY_REGIONS],
        reserved_memory_count: 0,
        reserved_memory: [ReservedMemoryRange::EMPTY; MAX_RESERVED_MEMORY_RANGES],
        kernel_image,
    }
}

fn map_pixel_format(format: GopPixelFormat) -> PixelFormat {
    match format {
        GopPixelFormat::Rgb => PixelFormat::Rgb,
        GopPixelFormat::Bgr => PixelFormat::Bgr,
        _ => PixelFormat::Unknown,
    }
}

fn collect_memory_regions(boot_info: &mut BootInfo, memory_map: &impl MemoryMap) {
    for descriptor in memory_map.entries() {
        boot_info.memory_region_total += 1;
        if boot_info.memory_region_count >= MAX_MEMORY_REGIONS {
            continue;
        }

        boot_info.memory_regions[boot_info.memory_region_count] = MemoryRegion {
            start: descriptor.phys_start as u64,
            page_count: descriptor.page_count,
            kind: map_memory_type(descriptor.ty),
            attributes: descriptor.att.bits(),
        };
        boot_info.memory_region_count += 1;
    }
}

fn collect_reserved_ranges(
    boot_info: &mut BootInfo,
    loader_image: Option<(u64, u64)>,
    memory_map_buffer: Option<(u64, u64)>,
) {
    push_reserved_range(
        boot_info,
        0,
        LOW_MEMORY_RESERVE_BYTES,
        ReservedMemoryKind::LowMemory,
    );

    if let Some((base, size)) = loader_image {
        push_reserved_range(boot_info, base, size, ReservedMemoryKind::LoaderImage);
    }

    push_reserved_range(
        boot_info,
        boot_info.framebuffer.base as u64,
        boot_info.framebuffer.size as u64,
        ReservedMemoryKind::FrameBuffer,
    );

    if let Some((base, size)) = memory_map_buffer {
        push_reserved_range(boot_info, base, size, ReservedMemoryKind::MemoryMap);
    }

    if boot_info.kernel_image.load_page_count != 0 {
        let start = align_down(boot_info.kernel_image.load_base);
        let length = boot_info.kernel_image.load_page_count.saturating_mul(PAGE_SIZE);
        push_reserved_range(boot_info, start, length, ReservedMemoryKind::KernelImageLoad);
    }
}

fn push_reserved_range(
    boot_info: &mut BootInfo,
    start: u64,
    length: u64,
    kind: ReservedMemoryKind,
) {
    if length == 0 || boot_info.reserved_memory_count >= MAX_RESERVED_MEMORY_RANGES {
        return;
    }

    boot_info.reserved_memory[boot_info.reserved_memory_count] = ReservedMemoryRange {
        start,
        length,
        kind,
    };
    boot_info.reserved_memory_count += 1;
}

fn log_boot_context(boot_info: &BootInfo) {
    kernel::log_line(format_args!(
        "boot mode: {}",
        boot_info.firmware_mode.as_str()
    ));
    kernel::log_line(format_args!(
        "reserved memory: {} ranges {} KiB",
        boot_info.reserved_memory_count,
        boot_info.reserved_memory_bytes() / 1024
    ));
    if boot_info.kernel_image.is_present() {
        kernel::log_line(format_args!(
            "kernel image: {} bytes entry=0x{:016x} staged=0x{:016x} phdrs={} load={}/{} staged={}",
            boot_info.kernel_image.image_size,
            boot_info.kernel_image.entry_point,
            boot_info.kernel_image.loaded_entry_point,
            boot_info.kernel_image.program_header_count,
            boot_info.kernel_image.load_segment_count,
            boot_info.kernel_image.load_segment_total,
            boot_info.kernel_image.loaded_segment_count
        ));
    } else {
        kernel::log_line(format_args!("kernel image: unavailable"));
    }
}

fn load_kernel_image() -> KernelImageInfo {
    let fs = match boot::get_image_file_system(boot::image_handle()) {
        Ok(fs) => fs,
        Err(_) => return KernelImageInfo::EMPTY,
    };
    let mut fs = FileSystem::new(fs);
    let path = match CString16::try_from("\\KERNEL.ELF") {
        Ok(path) => path,
        Err(_) => return KernelImageInfo::EMPTY,
    };
    let bytes = match fs.read(path.as_ref()) {
        Ok(bytes) => bytes,
        Err(_) => return KernelImageInfo::EMPTY,
    };

    let mut info = match parse_kernel_image(&bytes) {
        Some(info) => info,
        None => return KernelImageInfo::EMPTY,
    };
    stage_kernel_segments(&mut info, &bytes);
    info
}

fn parse_kernel_image(bytes: &[u8]) -> Option<KernelImageInfo> {
    let elf = ElfFile::new(bytes).ok()?;
    let mut info = KernelImageInfo {
        image_size: bytes.len() as u64,
        entry_point: elf.header.pt2.entry_point(),
        loaded_entry_point: 0,
        load_base: 0,
        load_page_count: 0,
        program_header_count: elf.header.pt2.ph_count() as usize,
        load_segment_count: 0,
        load_segment_total: 0,
        loaded_segment_count: 0,
        segments: [KernelImageSegment::EMPTY; MAX_KERNEL_IMAGE_SEGMENTS],
    };

    for header in elf.program_iter() {
        if !matches!(header.get_type().ok(), Some(ProgramType::Load)) {
            continue;
        }

        let segment = KernelImageSegment {
            virtual_address: header.virtual_addr(),
            physical_address: header.physical_addr(),
            file_offset: header.offset(),
            file_size: header.file_size(),
            memory_size: header.mem_size(),
            flags: encode_segment_flags(header.flags()),
            load_address: 0,
            load_page_count: 0,
        };

        if info.load_segment_count < MAX_KERNEL_IMAGE_SEGMENTS {
            info.segments[info.load_segment_count] = segment;
            info.load_segment_count += 1;
        }
        info.load_segment_total += 1;
    }

    Some(info)
}

fn encode_segment_flags(flags: xmas_elf::program::Flags) -> u32 {
    let mut encoded = 0_u32;
    if flags.is_read() {
        encoded |= KERNEL_SEGMENT_FLAG_READ;
    }
    if flags.is_write() {
        encoded |= KERNEL_SEGMENT_FLAG_WRITE;
    }
    if flags.is_execute() {
        encoded |= KERNEL_SEGMENT_FLAG_EXECUTE;
    }
    encoded
}

fn chainload_kernel(boot_info: &BootInfo) -> Status {
    let entry = boot_info.kernel_image.loaded_entry_point;
    if entry == 0 {
        return Status::LOAD_ERROR;
    }

    unsafe {
        let entry_fn: extern "sysv64" fn(*const BootInfo) -> ! =
            mem::transmute(entry as usize);
        entry_fn(boot_info as *const BootInfo);
    }
}

fn stage_kernel_segments(info: &mut KernelImageInfo, bytes: &[u8]) {
    let Some((image_base, image_page_count, allocation_phys)) =
        stage_kernel_image_span(info.segments(), bytes)
    else {
        return;
    };

    info.load_base = allocation_phys;
    info.load_page_count = image_page_count;
    info.loaded_entry_point = allocation_phys.saturating_add(info.entry_point.saturating_sub(image_base));

    for index in 0..info.load_segment_count {
        let segment = info.segments[index];
        let segment_base = align_down(segment.virtual_address);
        let page_offset = segment.virtual_address.saturating_sub(segment_base);
        let span_bytes = align_up(page_offset.saturating_add(segment.memory_size));
        let segment_pages = span_bytes / PAGE_SIZE;
        let load_address = allocation_phys
            .saturating_add(segment.virtual_address.saturating_sub(image_base));
        info.segments[index] = KernelImageSegment {
            load_address,
            load_page_count: segment_pages,
            ..segment
        };
        info.loaded_segment_count += 1;
    }
}

fn stage_kernel_image_span(
    segments: &[KernelImageSegment],
    bytes: &[u8],
) -> Option<(u64, u64, u64)> {
    let mut image_base = u64::MAX;
    let mut image_end = 0_u64;

    for segment in segments {
        if segment.memory_size == 0 {
            continue;
        }

        image_base = image_base.min(align_down(segment.virtual_address));
        image_end = image_end.max(align_up(segment.virtual_address.saturating_add(segment.memory_size)));
    }
    if image_base == u64::MAX || image_end <= image_base {
        return None;
    }

    let span_bytes = image_end.saturating_sub(image_base);
    let page_count = span_bytes / PAGE_SIZE;
    let allocation = allocate_kernel_pages(image_base, page_count as usize)?;
    let allocation_phys = allocation.as_ptr() as u64;

    unsafe {
        write_bytes(allocation.as_ptr(), 0, span_bytes as usize);
    }

    for segment in segments {
        if segment.memory_size == 0 {
            continue;
        }

        let end = segment.file_offset.checked_add(segment.file_size)?;
        if end > bytes.len() as u64 {
            return None;
        }

        let destination_offset = segment.virtual_address.saturating_sub(image_base);
        unsafe {
            let source = bytes.as_ptr().add(segment.file_offset as usize);
            let destination = allocation.as_ptr().add(destination_offset as usize);
            copy_nonoverlapping(source, destination, segment.file_size as usize);
        }
    }

    Some((image_base, page_count, allocation_phys))
}

fn allocate_kernel_pages(base: u64, page_count: usize) -> Option<core::ptr::NonNull<u8>> {
    let memory_type = uefi::mem::memory_map::MemoryType::LOADER_DATA;

    if base >= LOW_MEMORY_RESERVE_BYTES {
        if let Ok(pages) = boot::allocate_pages(AllocateType::Address(base), memory_type, page_count)
        {
            return Some(pages);
        }
    }

    boot::allocate_pages(AllocateType::AnyPages, memory_type, page_count).ok()
}

fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

fn align_up(value: u64) -> u64 {
    value.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn map_memory_type(memory_type: MemoryType) -> MemoryRegionKind {
    match memory_type {
        MemoryType::RESERVED => MemoryRegionKind::Reserved,
        MemoryType::LOADER_CODE => MemoryRegionKind::LoaderCode,
        MemoryType::LOADER_DATA => MemoryRegionKind::LoaderData,
        MemoryType::BOOT_SERVICES_CODE => MemoryRegionKind::BootServicesCode,
        MemoryType::BOOT_SERVICES_DATA => MemoryRegionKind::BootServicesData,
        MemoryType::RUNTIME_SERVICES_CODE => MemoryRegionKind::RuntimeServicesCode,
        MemoryType::RUNTIME_SERVICES_DATA => MemoryRegionKind::RuntimeServicesData,
        MemoryType::CONVENTIONAL => MemoryRegionKind::Conventional,
        MemoryType::UNUSABLE => MemoryRegionKind::Unusable,
        MemoryType::ACPI_RECLAIM => MemoryRegionKind::AcpiReclaim,
        MemoryType::ACPI_NON_VOLATILE => MemoryRegionKind::AcpiNonVolatile,
        MemoryType::MMIO => MemoryRegionKind::Mmio,
        MemoryType::MMIO_PORT_SPACE => MemoryRegionKind::MmioPortSpace,
        MemoryType::PAL_CODE => MemoryRegionKind::PalCode,
        MemoryType::PERSISTENT_MEMORY => MemoryRegionKind::PersistentMemory,
        MemoryType::UNACCEPTED => MemoryRegionKind::Unaccepted,
        _ => MemoryRegionKind::Unknown,
    }
}

fn translate_key(key: Key) -> Option<DesktopInput> {
    match key {
        Key::Special(ScanCode::LEFT) => Some(DesktopInput::MoveLeft),
        Key::Special(ScanCode::RIGHT) => Some(DesktopInput::MoveRight),
        Key::Special(ScanCode::UP) => Some(DesktopInput::MoveUp),
        Key::Special(ScanCode::DOWN) => Some(DesktopInput::MoveDown),
        Key::Special(ScanCode::ESCAPE) => Some(DesktopInput::Exit),
        Key::Printable(ch) => translate_printable(ch),
        _ => None,
    }
}

fn scale_pointer_delta(delta: i32) -> i32 {
    if delta == 0 {
        0
    } else {
        let magnitude = (delta.abs() / 250).clamp(1, 24);
        magnitude * delta.signum()
    }
}

fn translate_printable(ch: uefi::Char16) -> Option<DesktopInput> {
    let value = u16::from(ch);
    match value {
        0x0008 => Some(DesktopInput::Backspace),
        0x0009 => Some(DesktopInput::CycleFocus),
        0x000D => Some(DesktopInput::Submit),
        _ => core::char::from_u32(value as u32).map(DesktopInput::Character),
    }
}
