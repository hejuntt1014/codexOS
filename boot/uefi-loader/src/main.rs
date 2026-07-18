#![no_std]
#![no_main]

extern crate alloc;

mod allocator;

use alloc::boxed::Box;
use boot_runtime::{activate_post_ebs_vm, init, log_line};
use bootinfo::{
    BootInfo, FirmwareMode, FrameBufferInfo, KERNEL_SEGMENT_FLAG_EXECUTE, KERNEL_SEGMENT_FLAG_READ,
    KERNEL_SEGMENT_FLAG_WRITE, KernelImageInfo, KernelImageSegment, MAX_KERNEL_IMAGE_SEGMENTS,
    MAX_MEMORY_REGIONS, MAX_RESERVED_MEMORY_RANGES, MemoryRegion, MemoryRegionKind, PAGE_SIZE,
    PixelFormat, ReservedMemoryKind, ReservedMemoryRange,
};
use codex_release::{
    BootState, KEY_ID_BYTES, ManifestError, PUBLIC_KEY_BYTES, SystemSlot, TRUST_ROOT_STATE_BYTES,
    TrustRootState, UpdateManifest,
};
use core::ptr::{copy_nonoverlapping, write_bytes};
use core::time::Duration;
use core::{fmt, mem};
use desktop_runtime::{DesktopApp, DesktopInput, PointerSample};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use uefi::boot::{self, AllocateType};
use uefi::fs::FileSystem;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};
use uefi::proto::console::pointer::Pointer;
use uefi::proto::console::text::{Input, Key, ScanCode};
use uefi::proto::loaded_image::LoadedImage;
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{CString16, guid};
use xmas_elf::ElfFile;
use xmas_elf::program::Type as ProgramType;
use xmas_elf::sections::SectionData;

const LOW_MEMORY_RESERVE_BYTES: u64 = 1024 * 1024;
const RUNTIME_HHDM_BASE: u64 = 0xffff_8000_0000_0000;
const RUNTIME_HEAP_BYTES: usize = 64 * 1024 * 1024;
const KERNEL_HEAP_BYTES: usize = 64 * 1024 * 1024;
const CODEXOS_VARIABLE_VENDOR: VariableVendor =
    VariableVendor(guid!("38d5e429-307f-4f7a-9d27-bd21d55f4b92"));

#[global_allocator]
static ALLOCATOR: allocator::LoaderHeap = allocator::LoaderHeap::new();

#[entry]
fn main() -> Status {
    boot_runtime::serial::init();
    let runtime_heap = match initialize_runtime_heap() {
        Some(range) => range,
        None => return Status::OUT_OF_RESOURCES,
    };
    let kernel_heap = if cfg!(feature = "chainload") {
        match allocate_kernel_heap() {
            Some(range) => Some(range),
            None => return Status::OUT_OF_RESOURCES,
        }
    } else {
        None
    };

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
    let kernel_image = match load_kernel_image() {
        Ok(image) => image,
        Err(error) => {
            log_line(format_args!("kernel security failure: {:?}", error));
            return Status::SECURITY_VIOLATION;
        }
    };

    if cfg!(feature = "handoff") {
        return run_handoff_mode(
            gop,
            framebuffer,
            loader_image,
            kernel_image,
            runtime_heap,
            kernel_heap,
        );
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
        Some(runtime_heap),
        kernel_heap,
        Some((
            memory_map.buffer().as_ptr() as u64,
            memory_map.buffer().len() as u64,
        )),
    );

    init(&boot_info);
    log_heap_status();
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
        log_line(format_args!("pointer input: available"));
    } else {
        log_line(format_args!("pointer input: unavailable"));
    }

    let mut desktop = DesktopApp::new(&boot_info);

    loop {
        let mut had_input = false;

        while let Ok(Some(key)) = input.read_key() {
            if let Some(action) = translate_key(key) {
                desktop.handle_input(action);
                had_input = true;
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
                had_input = true;
            }
        }

        let mut rendered = false;
        if desktop.needs_redraw() {
            desktop.render(&boot_info);
            rendered = true;
        }

        if desktop.should_exit() {
            break;
        }

        let idle_delay_ms = if had_input {
            1
        } else if rendered {
            4
        } else {
            8
        };
        boot::stall(Duration::from_millis(idle_delay_ms));
    }

    Status::SUCCESS
}

fn initialize_runtime_heap() -> Option<(u64, u64)> {
    let page_size = PAGE_SIZE as usize;
    let page_count = RUNTIME_HEAP_BYTES.div_ceil(page_size);
    let allocation =
        boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, page_count).ok()?;
    let capacity = page_count.checked_mul(page_size)?;

    if ALLOCATOR
        .initialize_external(allocation.as_ptr(), capacity)
        .is_err()
    {
        unsafe {
            let _ = boot::free_pages(allocation, page_count);
        }
        return None;
    }

    Some((allocation.as_ptr() as u64, capacity as u64))
}

fn allocate_kernel_heap() -> Option<(u64, u64)> {
    let page_size = PAGE_SIZE as usize;
    let page_count = KERNEL_HEAP_BYTES.div_ceil(page_size);
    let allocation =
        boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, page_count).ok()?;
    let capacity = page_count.checked_mul(page_size)?;
    Some((allocation.as_ptr() as u64, capacity as u64))
}

fn log_heap_status() {
    let stats = ALLOCATOR.stats();
    log_line(format_args!(
        "heap: capacity={} MiB used={} KiB peak={} KiB free={} MiB live={} failed={}",
        stats.capacity_bytes / (1024 * 1024),
        stats.used_bytes / 1024,
        stats.peak_used_bytes / 1024,
        stats.free_bytes / (1024 * 1024),
        stats.live_allocations,
        stats.failed_allocations
    ));
}

fn run_handoff_mode(
    gop: uefi::boot::ScopedProtocol<GraphicsOutput>,
    framebuffer: FrameBufferInfo,
    loader_image: Option<(u64, u64)>,
    kernel_image: KernelImageInfo,
    runtime_heap: (u64, u64),
    kernel_heap: Option<(u64, u64)>,
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
        Some(runtime_heap),
        kernel_heap,
        Some((
            memory_map.buffer().as_ptr() as u64,
            memory_map.buffer().len() as u64,
        )),
    );
    let boot_info = Box::leak(Box::new(boot_info));

    init(boot_info);
    log_heap_status();
    log_boot_context(boot_info);
    if let Err(error) = activate_post_ebs_vm(boot_info) {
        log_line(format_args!("vm activation failed: {:?}", error));
        boot_runtime::interrupts::halt();
    }

    if cfg!(feature = "chainload") {
        return chainload_kernel(boot_info);
    }

    let mut desktop = DesktopApp::new(boot_info);
    desktop.note_handoff_complete();
    desktop.render(boot_info);

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
            start: descriptor.phys_start,
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
    runtime_heap: Option<(u64, u64)>,
    kernel_heap: Option<(u64, u64)>,
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

    if let Some((base, size)) = runtime_heap {
        push_reserved_range(boot_info, base, size, ReservedMemoryKind::RuntimeHeap);
    }

    if let Some((base, size)) = kernel_heap {
        push_reserved_range(boot_info, base, size, ReservedMemoryKind::KernelHeap);
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
        let length = boot_info
            .kernel_image
            .load_page_count
            .saturating_mul(PAGE_SIZE);
        push_reserved_range(
            boot_info,
            start,
            length,
            ReservedMemoryKind::KernelImageLoad,
        );
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
    log_line(format_args!(
        "boot mode: {}",
        boot_info.firmware_mode.as_str()
    ));
    log_line(format_args!(
        "reserved memory: {} ranges {} KiB",
        boot_info.reserved_memory_count,
        boot_info.reserved_memory_bytes() / 1024
    ));
    if boot_info.kernel_image.is_present() {
        log_line(format_args!(
            "kernel image: {} bytes entry=0x{:016x} staged=0x{:016x} phdrs={} load={}/{} staged={} relocs={}",
            boot_info.kernel_image.image_size,
            boot_info.kernel_image.entry_point,
            boot_info.kernel_image.loaded_entry_point,
            boot_info.kernel_image.program_header_count,
            boot_info.kernel_image.load_segment_count,
            boot_info.kernel_image.load_segment_total,
            boot_info.kernel_image.loaded_segment_count,
            boot_info.kernel_image.relocation_count
        ));
        let key_id = boot_info.kernel_image.verification_key_id;
        let hash = boot_info.kernel_image.kernel_sha256;
        let slot = if boot_info.kernel_image.system_slot == 0 {
            "A"
        } else {
            "B"
        };
        log_line(format_args!(
            "kernel signature: verified={} version={} slot={} state-gen={} recovery={} key-id={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} sha256-prefix={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            boot_info.kernel_image.signature_verified == 1,
            boot_info.kernel_image.release_version,
            slot,
            boot_info.kernel_image.boot_state_generation,
            boot_info.kernel_image.recovery_fallback == 1,
            key_id[0],
            key_id[1],
            key_id[2],
            key_id[3],
            key_id[4],
            key_id[5],
            key_id[6],
            key_id[7],
            key_id[8],
            key_id[9],
            key_id[10],
            key_id[11],
            key_id[12],
            key_id[13],
            key_id[14],
            key_id[15],
            hash[0],
            hash[1],
            hash[2],
            hash[3],
            hash[4],
            hash[5],
            hash[6],
            hash[7]
        ));
    } else {
        log_line(format_args!("kernel image: unavailable"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KernelLoadError {
    FileSystemUnavailable,
    BootStateUnavailable,
    BootStateRepairFailed,
    BootStateGenerationExhausted,
    InvalidPath,
    KernelReadFailed,
    ManifestReadFailed,
    ManifestInvalid(ManifestError),
    KernelSizeMismatch,
    KernelHashMismatch,
    TrustedKeyUnavailable,
    PersistedTrustRootInvalid,
    PersistedTrustRootUnavailable,
    NextTrustKeyInvalid,
    NextTrustKeyIdMismatch,
    TrustRootUpdateFailed,
    KeyIdMismatch,
    SignatureInvalid,
    RollbackStateUnavailable,
    RollbackDetected { candidate: u64, minimum: u64 },
    BootStateReleaseMismatch,
    BothSystemSlotsUnavailable,
    InvalidElf,
    StagingFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustRootSource {
    Embedded,
    Persisted,
}

impl TrustRootSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Persisted => "persisted",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TrustRoot {
    public_key: [u8; PUBLIC_KEY_BYTES],
    key_id: [u8; KEY_ID_BYTES],
    source: TrustRootSource,
    activation_release_version: u64,
}

#[derive(Debug, Clone, Copy)]
struct VerifiedRelease {
    trust_root: TrustRoot,
}

struct HexBytes<'a>(&'a [u8]);

impl fmt::Display for HexBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn load_kernel_image() -> Result<KernelImageInfo, KernelLoadError> {
    let fs = match boot::get_image_file_system(boot::image_handle()) {
        Ok(fs) => fs,
        Err(_) => return Err(KernelLoadError::FileSystemUnavailable),
    };
    let mut fs = FileSystem::new(fs);
    let state = match load_boot_state(&mut fs) {
        Ok(state) => state,
        Err(KernelLoadError::BootStateUnavailable) => {
            return recover_from_missing_boot_state(&mut fs);
        }
        Err(error) => return Err(error),
    };
    match load_system_slot(&mut fs, state.active_slot) {
        Ok(mut image) if image.release_version == state.active_release_version => {
            image.boot_state_generation = state.generation;
            image.system_slot = state.active_slot.as_u8();
            return Ok(image);
        }
        Ok(_) => log_line(format_args!(
            "system slot {} rejected: {:?}",
            state.active_slot.as_str(),
            KernelLoadError::BootStateReleaseMismatch
        )),
        Err(error) => log_line(format_args!(
            "system slot {} rejected: {:?}",
            state.active_slot.as_str(),
            error
        )),
    }

    let fallback = state.active_slot.other();
    match load_system_slot(&mut fs, fallback) {
        Ok(mut image) => {
            image.system_slot = fallback.as_u8();
            image.recovery_fallback = 1;
            log_line(format_args!(
                "system recovery fallback: active={} selected={} version={}",
                state.active_slot.as_str(),
                fallback.as_str(),
                image.release_version
            ));
            match persist_fallback_boot_state(&mut fs, state, fallback, image.release_version) {
                Ok(repaired) => {
                    image.boot_state_generation = repaired.generation;
                    log_line(format_args!(
                        "system recovery state repair: selected={} version={} generation={} verified=true",
                        fallback.as_str(),
                        image.release_version,
                        repaired.generation
                    ));
                }
                Err(error) => {
                    image.boot_state_generation = state.generation;
                    log_line(format_args!(
                        "system recovery state repair: selected={} version={} generation={} verified=false error={:?}",
                        fallback.as_str(),
                        image.release_version,
                        state.generation,
                        error
                    ));
                }
            }
            Ok(image)
        }
        Err(error) => {
            log_line(format_args!(
                "system slot {} rejected: {:?}",
                fallback.as_str(),
                error
            ));
            Err(KernelLoadError::BothSystemSlotsUnavailable)
        }
    }
}

fn recover_from_missing_boot_state(
    fs: &mut FileSystem,
) -> Result<KernelImageInfo, KernelLoadError> {
    let mut selected: Option<(SystemSlot, KernelImageInfo)> = None;
    for slot in [SystemSlot::A, SystemSlot::B] {
        match load_system_slot(fs, slot) {
            Ok(image) => {
                if selected
                    .as_ref()
                    .is_none_or(|(_, current)| image.release_version > current.release_version)
                {
                    selected = Some((slot, image));
                }
            }
            Err(error) => log_line(format_args!(
                "system slot {} rejected during boot-state recovery: {:?}",
                slot.as_str(),
                error
            )),
        }
    }

    let Some((slot, mut image)) = selected else {
        return Err(KernelLoadError::BothSystemSlotsUnavailable);
    };
    image.system_slot = slot.as_u8();
    image.recovery_fallback = 1;
    match repair_boot_state(fs, slot, image.release_version) {
        Ok(state) => {
            image.boot_state_generation = state.generation;
            log_line(format_args!(
                "system boot-state recovery: selected={} version={} source=signed-slot-scan repaired=true generation={}",
                slot.as_str(),
                image.release_version,
                state.generation
            ));
        }
        Err(error) => {
            image.boot_state_generation = 0;
            log_line(format_args!(
                "system boot-state recovery: selected={} version={} source=signed-slot-scan repaired=false error={:?}",
                slot.as_str(),
                image.release_version,
                error
            ));
        }
    }
    Ok(image)
}

fn repair_boot_state(
    fs: &mut FileSystem,
    slot: SystemSlot,
    release_version: u64,
) -> Result<BootState, KernelLoadError> {
    let first = BootState {
        generation: 1,
        active_slot: slot,
        active_release_version: release_version,
    };
    let second = BootState {
        generation: 2,
        ..first
    };
    for (path, state) in [("\\BOOTSTA0.BIN", first), ("\\BOOTSTA1.BIN", second)] {
        write_verified_boot_state(fs, path, state)?;
    }
    log_line(format_args!(
        "system boot-state repair: selected={} version={} generation=2 copies=2 verified=true",
        slot.as_str(),
        release_version
    ));
    Ok(second)
}

fn persist_fallback_boot_state(
    fs: &mut FileSystem,
    current: BootState,
    fallback: SystemSlot,
    release_version: u64,
) -> Result<BootState, KernelLoadError> {
    let generation = current
        .generation
        .checked_add(1)
        .ok_or(KernelLoadError::BootStateGenerationExhausted)?;
    let repaired = BootState {
        generation,
        active_slot: fallback,
        active_release_version: release_version,
    };
    let path = if generation & 1 == 1 {
        "\\BOOTSTA0.BIN"
    } else {
        "\\BOOTSTA1.BIN"
    };
    write_verified_boot_state(fs, path, repaired)?;
    Ok(repaired)
}

fn write_verified_boot_state(
    fs: &mut FileSystem,
    path: &str,
    state: BootState,
) -> Result<(), KernelLoadError> {
    let path = CString16::try_from(path).map_err(|_| KernelLoadError::InvalidPath)?;
    fs.write(path.as_ref(), state.encode())
        .map_err(|_| KernelLoadError::BootStateRepairFailed)?;
    let bytes = fs
        .read(path.as_ref())
        .map_err(|_| KernelLoadError::BootStateRepairFailed)?;
    if BootState::decode(&bytes).ok() != Some(state) {
        return Err(KernelLoadError::BootStateRepairFailed);
    }
    Ok(())
}

fn load_boot_state(fs: &mut FileSystem) -> Result<BootState, KernelLoadError> {
    let mut selected: Option<BootState> = None;
    for path in ["\\BOOTSTA0.BIN", "\\BOOTSTA1.BIN"] {
        let path = CString16::try_from(path).map_err(|_| KernelLoadError::InvalidPath)?;
        let Ok(bytes) = fs.read(path.as_ref()) else {
            continue;
        };
        let Ok(state) = BootState::decode(&bytes) else {
            continue;
        };
        if selected
            .as_ref()
            .is_none_or(|current| state.generation > current.generation)
        {
            selected = Some(state);
        }
    }
    selected.ok_or(KernelLoadError::BootStateUnavailable)
}

fn load_system_slot(
    fs: &mut FileSystem,
    slot: SystemSlot,
) -> Result<KernelImageInfo, KernelLoadError> {
    let (kernel_path, manifest_path) = match slot {
        SystemSlot::A => ("\\SYSTEM\\A\\KERNEL.ELF", "\\SYSTEM\\A\\KERNEL.SIG"),
        SystemSlot::B => ("\\SYSTEM\\B\\KERNEL.ELF", "\\SYSTEM\\B\\KERNEL.SIG"),
    };
    let kernel_path = CString16::try_from(kernel_path).map_err(|_| KernelLoadError::InvalidPath)?;
    let bytes = fs
        .read(kernel_path.as_ref())
        .map_err(|_| KernelLoadError::KernelReadFailed)?;
    let manifest_path =
        CString16::try_from(manifest_path).map_err(|_| KernelLoadError::InvalidPath)?;
    let manifest_bytes = fs
        .read(manifest_path.as_ref())
        .map_err(|_| KernelLoadError::ManifestReadFailed)?;
    let manifest =
        UpdateManifest::decode(&manifest_bytes).map_err(KernelLoadError::ManifestInvalid)?;
    let verified = verify_kernel_release(&bytes, &manifest)?;

    let mut info = parse_kernel_image(&bytes).ok_or(KernelLoadError::InvalidElf)?;
    info.release_version = manifest.release_version;
    info.signature_verified = 1;
    info.verification_key_id = manifest.key_id;
    info.kernel_sha256 = manifest.kernel_sha256;
    if !stage_kernel_segments(&mut info, &bytes) {
        return Err(KernelLoadError::StagingFailed);
    }
    enforce_release_version(manifest.release_version)?;
    apply_trust_root_update(&manifest, &verified.trust_root)?;
    Ok(info)
}

fn verify_kernel_release(
    kernel: &[u8],
    manifest: &UpdateManifest,
) -> Result<VerifiedRelease, KernelLoadError> {
    if u64::try_from(kernel.len()).ok() != Some(manifest.kernel_size) {
        return Err(KernelLoadError::KernelSizeMismatch);
    }
    let kernel_sha256: [u8; 32] = Sha256::digest(kernel).into();
    if kernel_sha256 != manifest.kernel_sha256 {
        return Err(KernelLoadError::KernelHashMismatch);
    }
    let trust_root = active_trust_root()?;
    let verifying_key = VerifyingKey::from_bytes(&trust_root.public_key)
        .map_err(|_| KernelLoadError::TrustedKeyUnavailable)?;
    if trust_root.key_id != manifest.key_id {
        return Err(KernelLoadError::KeyIdMismatch);
    }
    let signature = Signature::from_bytes(&manifest.signature);
    let signing_bytes = manifest.signing_bytes();
    verifying_key
        .verify_strict(&signing_bytes[..manifest.signing_len()], &signature)
        .map_err(|_| KernelLoadError::SignatureInvalid)?;
    log_line(format_args!(
        "kernel trust root: source={} activation-version={} signer-key-id={}",
        trust_root.source.as_str(),
        trust_root.activation_release_version,
        HexBytes(&trust_root.key_id)
    ));
    Ok(VerifiedRelease { trust_root })
}

fn active_trust_root() -> Result<TrustRoot, KernelLoadError> {
    let embedded_public_key =
        embedded_trusted_public_key().ok_or(KernelLoadError::TrustedKeyUnavailable)?;
    let embedded = TrustRoot {
        public_key: embedded_public_key,
        key_id: key_id_for_public_key(&embedded_public_key),
        source: TrustRootSource::Embedded,
        activation_release_version: 0,
    };

    let name = CString16::try_from("CodexOsTrustRoot")
        .map_err(|_| KernelLoadError::PersistedTrustRootUnavailable)?;
    let mut buffer = [0_u8; TRUST_ROOT_STATE_BYTES];
    match runtime::get_variable(name.as_ref(), &CODEXOS_VARIABLE_VENDOR, &mut buffer) {
        Ok((value, _)) if value.len() == TRUST_ROOT_STATE_BYTES => {
            let state = TrustRootState::decode(value)
                .map_err(|_| KernelLoadError::PersistedTrustRootInvalid)?;
            VerifyingKey::from_bytes(&state.public_key)
                .map_err(|_| KernelLoadError::PersistedTrustRootInvalid)?;
            if key_id_for_public_key(&state.public_key) != state.key_id {
                return Err(KernelLoadError::PersistedTrustRootInvalid);
            }
            Ok(TrustRoot {
                public_key: state.public_key,
                key_id: state.key_id,
                source: TrustRootSource::Persisted,
                activation_release_version: state.activation_release_version,
            })
        }
        Ok(_) => Err(KernelLoadError::PersistedTrustRootInvalid),
        Err(error) if error.status() == Status::NOT_FOUND => Ok(embedded),
        Err(_) => Err(KernelLoadError::PersistedTrustRootUnavailable),
    }
}

fn apply_trust_root_update(
    manifest: &UpdateManifest,
    current: &TrustRoot,
) -> Result<(), KernelLoadError> {
    if !manifest.has_next_trust_key() {
        log_line(format_args!(
            "trust root update: none source={} signer-key-id={}",
            current.source.as_str(),
            HexBytes(&current.key_id)
        ));
        return Ok(());
    }

    let next_public_key = manifest.next_trust_public_key;
    VerifyingKey::from_bytes(&next_public_key).map_err(|_| KernelLoadError::NextTrustKeyInvalid)?;
    let next_key_id = key_id_for_public_key(&next_public_key);
    if next_key_id != manifest.next_trust_key_id {
        return Err(KernelLoadError::NextTrustKeyIdMismatch);
    }

    if next_public_key == current.public_key {
        log_line(format_args!(
            "trust root update: retained version={} key-id={}",
            manifest.release_version,
            HexBytes(&next_key_id)
        ));
        return Ok(());
    }

    let state = TrustRootState {
        activation_release_version: manifest.release_version,
        public_key: next_public_key,
        key_id: next_key_id,
    };
    let name = CString16::try_from("CodexOsTrustRoot")
        .map_err(|_| KernelLoadError::TrustRootUpdateFailed)?;
    let attributes = VariableAttributes::NON_VOLATILE
        | VariableAttributes::BOOTSERVICE_ACCESS
        | VariableAttributes::RUNTIME_ACCESS;
    runtime::set_variable(
        name.as_ref(),
        &CODEXOS_VARIABLE_VENDOR,
        attributes,
        &state.encode(),
    )
    .map_err(|_| KernelLoadError::TrustRootUpdateFailed)?;

    let mut buffer = [0_u8; TRUST_ROOT_STATE_BYTES];
    let (value, _) = runtime::get_variable(name.as_ref(), &CODEXOS_VARIABLE_VENDOR, &mut buffer)
        .map_err(|_| KernelLoadError::TrustRootUpdateFailed)?;
    if value.len() != TRUST_ROOT_STATE_BYTES || TrustRootState::decode(value).ok() != Some(state) {
        return Err(KernelLoadError::TrustRootUpdateFailed);
    }

    log_line(format_args!(
        "trust root update: activated version={} previous-source={} next-key-id={}",
        manifest.release_version,
        current.source.as_str(),
        HexBytes(&next_key_id)
    ));
    Ok(())
}

fn key_id_for_public_key(public_key: &[u8; PUBLIC_KEY_BYTES]) -> [u8; KEY_ID_BYTES] {
    let public_hash = Sha256::digest(public_key);
    let mut key_id = [0_u8; KEY_ID_BYTES];
    key_id.copy_from_slice(&public_hash[..KEY_ID_BYTES]);
    key_id
}

fn embedded_trusted_public_key() -> Option<[u8; PUBLIC_KEY_BYTES]> {
    let encoded = option_env!("CODEXOS_TRUSTED_PUBLIC_KEY_HEX")?;
    if encoded.len() != 64 {
        return None;
    }
    let mut key = [0_u8; PUBLIC_KEY_BYTES];
    for (index, byte) in key.iter_mut().enumerate() {
        let high = hex_nibble(encoded.as_bytes()[index * 2])?;
        let low = hex_nibble(encoded.as_bytes()[index * 2 + 1])?;
        *byte = high << 4 | low;
    }
    Some(key)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn enforce_release_version(candidate: u64) -> Result<(), KernelLoadError> {
    let name = CString16::try_from("CodexOsHighestRelease")
        .map_err(|_| KernelLoadError::RollbackStateUnavailable)?;
    let mut buffer = [0_u8; 8];
    let minimum = match runtime::get_variable(name.as_ref(), &CODEXOS_VARIABLE_VENDOR, &mut buffer)
    {
        Ok((value, _)) if value.len() == 8 => {
            u64::from_le_bytes(value.try_into().unwrap_or([0; 8]))
        }
        Ok(_) => return Err(KernelLoadError::RollbackStateUnavailable),
        Err(error) if error.status() == Status::NOT_FOUND => 0,
        Err(_) => return Err(KernelLoadError::RollbackStateUnavailable),
    };
    if candidate < minimum {
        return Err(KernelLoadError::RollbackDetected { candidate, minimum });
    }
    if candidate > minimum {
        let attributes = VariableAttributes::NON_VOLATILE
            | VariableAttributes::BOOTSERVICE_ACCESS
            | VariableAttributes::RUNTIME_ACCESS;
        runtime::set_variable(
            name.as_ref(),
            &CODEXOS_VARIABLE_VENDOR,
            attributes,
            &candidate.to_le_bytes(),
        )
        .map_err(|_| KernelLoadError::RollbackStateUnavailable)?;
    }
    Ok(())
}

fn parse_kernel_image(bytes: &[u8]) -> Option<KernelImageInfo> {
    let elf = ElfFile::new(bytes).ok()?;
    let mut info = KernelImageInfo {
        image_size: bytes.len() as u64,
        release_version: 0,
        boot_state_generation: 0,
        system_slot: 0,
        recovery_fallback: 0,
        signature_verified: 0,
        verification_key_id: [0; 16],
        kernel_sha256: [0; 32],
        entry_point: elf.header.pt2.entry_point(),
        loaded_entry_point: 0,
        load_base: 0,
        load_page_count: 0,
        program_header_count: elf.header.pt2.ph_count() as usize,
        load_segment_count: 0,
        load_segment_total: 0,
        loaded_segment_count: 0,
        relocation_count: 0,
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
    let entry = boot_info.kernel_image.entry_point;
    if entry == 0 {
        return Status::LOAD_ERROR;
    }

    unsafe {
        let entry_fn: unsafe extern "sysv64" fn(*const BootInfo) -> ! =
            mem::transmute(entry as usize);
        entry_fn(boot_info as *const BootInfo);
    }
}

fn stage_kernel_segments(info: &mut KernelImageInfo, bytes: &[u8]) -> bool {
    let Some((image_base, image_page_count, allocation_phys, relocation_count)) =
        stage_kernel_image_span(info.segments(), bytes)
    else {
        return false;
    };

    info.load_base = allocation_phys;
    info.load_page_count = image_page_count;
    info.loaded_entry_point =
        allocation_phys.saturating_add(info.entry_point.saturating_sub(image_base));
    info.relocation_count = relocation_count;

    for index in 0..info.load_segment_count {
        let segment = info.segments[index];
        let segment_base = align_down(segment.virtual_address);
        let page_offset = segment.virtual_address.saturating_sub(segment_base);
        let span_bytes = align_up(page_offset.saturating_add(segment.memory_size));
        let segment_pages = span_bytes / PAGE_SIZE;
        let load_address =
            allocation_phys.saturating_add(segment.virtual_address.saturating_sub(image_base));
        info.segments[index] = KernelImageSegment {
            load_address,
            load_page_count: segment_pages,
            ..segment
        };
        info.loaded_segment_count += 1;
    }
    true
}

fn stage_kernel_image_span(
    segments: &[KernelImageSegment],
    bytes: &[u8],
) -> Option<(u64, u64, u64, usize)> {
    let mut image_base = u64::MAX;
    let mut image_end = 0_u64;

    for segment in segments {
        if segment.memory_size == 0 {
            continue;
        }

        image_base = image_base.min(align_down(segment.virtual_address));
        image_end = image_end.max(align_up(
            segment.virtual_address.saturating_add(segment.memory_size),
        ));
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

    let relocation_count =
        apply_kernel_relocations(bytes, image_base, allocation_phys, span_bytes)?;
    Some((image_base, page_count, allocation_phys, relocation_count))
}

fn apply_kernel_relocations(
    bytes: &[u8],
    image_base: u64,
    allocation_phys: u64,
    span_bytes: u64,
) -> Option<usize> {
    const R_X86_64_RELATIVE: u32 = 8;

    let elf = ElfFile::new(bytes).ok()?;
    let mut applied = 0usize;
    for section in elf.section_iter() {
        match section.get_data(&elf).ok()? {
            SectionData::Rela64(relocations) => {
                for relocation in relocations {
                    if relocation.get_type() != R_X86_64_RELATIVE
                        || relocation.get_symbol_table_index() != 0
                    {
                        return None;
                    }

                    let target_offset = relocation.get_offset().checked_sub(image_base)?;
                    let target_end =
                        target_offset.checked_add(core::mem::size_of::<u64>() as u64)?;
                    if target_end > span_bytes {
                        return None;
                    }
                    let target_address = allocation_phys.checked_add(target_offset)?;
                    unsafe {
                        core::ptr::write_unaligned(
                            target_address as *mut u64,
                            relocation.get_addend(),
                        );
                    }
                    applied = applied.checked_add(1)?;
                }
            }
            SectionData::Rela32(_) => return None,
            _ => {}
        }
    }
    Some(applied)
}

fn allocate_kernel_pages(base: u64, page_count: usize) -> Option<core::ptr::NonNull<u8>> {
    let memory_type = uefi::mem::memory_map::MemoryType::LOADER_DATA;

    if base >= LOW_MEMORY_RESERVE_BYTES
        && let Ok(pages) =
            boot::allocate_pages(AllocateType::Address(base), memory_type, page_count)
    {
        return Some(pages);
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
