#![no_std]

pub const PAGE_SIZE: u64 = 4096;
pub const MAX_MEMORY_REGIONS: usize = 128;
pub const MAX_RESERVED_MEMORY_RANGES: usize = 16;
pub const MAX_KERNEL_IMAGE_SEGMENTS: usize = 8;
pub const KERNEL_SEGMENT_FLAG_READ: u32 = 1 << 0;
pub const KERNEL_SEGMENT_FLAG_WRITE: u32 = 1 << 1;
pub const KERNEL_SEGMENT_FLAG_EXECUTE: u32 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PixelFormat {
    Rgb = 0,
    Bgr = 1,
    Unknown = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FirmwareMode {
    UefiBootServices = 0,
    PostExitBootServices = 1,
}

impl FirmwareMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UefiBootServices => "uefi-boot-services",
            Self::PostExitBootServices => "post-exit-boot-services",
        }
    }

    pub const fn region_is_currently_usable(self, kind: MemoryRegionKind) -> bool {
        match self {
            Self::UefiBootServices => matches!(
                kind,
                MemoryRegionKind::Conventional
                    | MemoryRegionKind::LoaderCode
                    | MemoryRegionKind::LoaderData
            ),
            Self::PostExitBootServices => kind.is_reclaimable_by_kernel(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MemoryRegionKind {
    Reserved = 0,
    LoaderCode = 1,
    LoaderData = 2,
    BootServicesCode = 3,
    BootServicesData = 4,
    RuntimeServicesCode = 5,
    RuntimeServicesData = 6,
    Conventional = 7,
    Unusable = 8,
    AcpiReclaim = 9,
    AcpiNonVolatile = 10,
    Mmio = 11,
    MmioPortSpace = 12,
    PalCode = 13,
    PersistentMemory = 14,
    Unaccepted = 15,
    Unknown = u32::MAX,
}

impl MemoryRegionKind {
    pub const fn is_reclaimable_by_kernel(self) -> bool {
        matches!(
            self,
            Self::Conventional
                | Self::LoaderCode
                | Self::LoaderData
                | Self::BootServicesCode
                | Self::BootServicesData
                | Self::AcpiReclaim
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::LoaderCode => "loader-code",
            Self::LoaderData => "loader-data",
            Self::BootServicesCode => "boot-code",
            Self::BootServicesData => "boot-data",
            Self::RuntimeServicesCode => "runtime-code",
            Self::RuntimeServicesData => "runtime-data",
            Self::Conventional => "conventional",
            Self::Unusable => "unusable",
            Self::AcpiReclaim => "acpi-reclaim",
            Self::AcpiNonVolatile => "acpi-nvs",
            Self::Mmio => "mmio",
            Self::MmioPortSpace => "mmio-port",
            Self::PalCode => "pal",
            Self::PersistentMemory => "persistent",
            Self::Unaccepted => "unaccepted",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ReservedMemoryKind {
    LowMemory = 0,
    LoaderImage = 1,
    FrameBuffer = 2,
    MemoryMap = 3,
    KernelImageLoad = 4,
    Unknown = u32::MAX,
}

impl ReservedMemoryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LowMemory => "low-memory",
            Self::LoaderImage => "loader-image",
            Self::FrameBuffer => "framebuffer",
            Self::MemoryMap => "memory-map",
            Self::KernelImageLoad => "kernel-image-load",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FrameBufferInfo {
    pub base: *mut u8,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub pixel_format: PixelFormat,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryRegion {
    pub start: u64,
    pub page_count: u64,
    pub kind: MemoryRegionKind,
    pub attributes: u64,
}

impl MemoryRegion {
    pub const EMPTY: Self = Self {
        start: 0,
        page_count: 0,
        kind: MemoryRegionKind::Reserved,
        attributes: 0,
    };

    pub const fn size_bytes(self) -> u64 {
        self.page_count.saturating_mul(PAGE_SIZE)
    }

    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.size_bytes())
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ReservedMemoryRange {
    pub start: u64,
    pub length: u64,
    pub kind: ReservedMemoryKind,
}

impl ReservedMemoryRange {
    pub const EMPTY: Self = Self {
        start: 0,
        length: 0,
        kind: ReservedMemoryKind::Unknown,
    };

    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.length)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KernelImageSegment {
    pub virtual_address: u64,
    pub physical_address: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub flags: u32,
    pub load_address: u64,
    pub load_page_count: u64,
}

impl KernelImageSegment {
    pub const EMPTY: Self = Self {
        virtual_address: 0,
        physical_address: 0,
        file_offset: 0,
        file_size: 0,
        memory_size: 0,
        flags: 0,
        load_address: 0,
        load_page_count: 0,
    };
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KernelImageInfo {
    pub image_size: u64,
    pub entry_point: u64,
    pub loaded_entry_point: u64,
    pub load_base: u64,
    pub load_page_count: u64,
    pub program_header_count: usize,
    pub load_segment_count: usize,
    pub load_segment_total: usize,
    pub loaded_segment_count: usize,
    pub segments: [KernelImageSegment; MAX_KERNEL_IMAGE_SEGMENTS],
}

impl KernelImageInfo {
    pub const EMPTY: Self = Self {
        image_size: 0,
        entry_point: 0,
        loaded_entry_point: 0,
        load_base: 0,
        load_page_count: 0,
        program_header_count: 0,
        load_segment_count: 0,
        load_segment_total: 0,
        loaded_segment_count: 0,
        segments: [KernelImageSegment::EMPTY; MAX_KERNEL_IMAGE_SEGMENTS],
    };

    pub const fn is_present(self) -> bool {
        self.image_size != 0
    }

    pub const fn is_loaded(self) -> bool {
        self.loaded_segment_count != 0
    }

    pub fn segments(&self) -> &[KernelImageSegment] {
        &self.segments[..self.load_segment_count.min(MAX_KERNEL_IMAGE_SEGMENTS)]
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BootInfo {
    pub framebuffer: FrameBufferInfo,
    pub firmware_mode: FirmwareMode,
    pub runtime_hhdm_base: u64,
    pub memory_region_count: usize,
    pub memory_region_total: usize,
    pub memory_regions: [MemoryRegion; MAX_MEMORY_REGIONS],
    pub reserved_memory_count: usize,
    pub reserved_memory: [ReservedMemoryRange; MAX_RESERVED_MEMORY_RANGES],
    pub kernel_image: KernelImageInfo,
}

impl BootInfo {
    pub fn memory_regions(&self) -> &[MemoryRegion] {
        &self.memory_regions[..self.memory_region_count.min(MAX_MEMORY_REGIONS)]
    }

    pub fn total_memory_bytes(&self) -> u64 {
        self.memory_regions()
            .iter()
            .fold(0_u64, |sum, region| sum.saturating_add(region.size_bytes()))
    }

    pub fn reserved_memory(&self) -> &[ReservedMemoryRange] {
        &self.reserved_memory[..self.reserved_memory_count.min(MAX_RESERVED_MEMORY_RANGES)]
    }

    pub fn reserved_range(&self, kind: ReservedMemoryKind) -> Option<ReservedMemoryRange> {
        self.reserved_memory()
            .iter()
            .copied()
            .find(|range| range.kind == kind)
    }

    pub fn reserved_memory_bytes(&self) -> u64 {
        self.reserved_memory()
            .iter()
            .fold(0_u64, |sum, range| sum.saturating_add(range.length))
    }

    pub fn usable_memory_bytes(&self) -> u64 {
        self.memory_regions().iter().fold(0_u64, |sum, region| {
            if self.firmware_mode.region_is_currently_usable(region.kind) {
                sum.saturating_add(region.size_bytes())
            } else {
                sum
            }
        })
    }
}
