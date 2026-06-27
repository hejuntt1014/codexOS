use core::sync::atomic::{Ordering, fence};

use crate::{hardware, memory, vm};

pub const SECTOR_SIZE: usize = 512;
const VIRTIO_HOST_FEATURES: u16 = 0x00;
const VIRTIO_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_QUEUE_PFN: u16 = 0x08;
const VIRTIO_QUEUE_SIZE: u16 = 0x0c;
const VIRTIO_QUEUE_SELECT: u16 = 0x0e;
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_DEVICE_STATUS: u16 = 0x12;
const VIRTIO_ISR_STATUS: u16 = 0x13;
const VIRTIO_DEVICE_CONFIG: u16 = 0x14;
const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FAILED: u8 = 128;
const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9;
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const REQUEST_HEADER_OFFSET: u64 = 0;
const REQUEST_STATUS_OFFSET: u64 = 16;
const REQUEST_DATA_OFFSET: u64 = SECTOR_SIZE as u64;
const REQUEST_TIMEOUT_SPINS: usize = 20_000_000;

pub trait BlockDevice {
    fn sector_count(&self) -> u64;
    fn supports_flush(&self) -> bool;
    fn read_sector(
        &mut self,
        sector: u64,
        output: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlockError>;
    fn write_sector(&mut self, sector: u64, input: &[u8; SECTOR_SIZE]) -> Result<(), BlockError>;
    fn flush(&mut self) -> Result<(), BlockError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    DeviceNotFound,
    InvalidIoBar,
    QueueUnavailable,
    QueueTooSmall,
    QueueAddressTooHigh,
    DmaAddressUnavailable,
    DeviceRejected,
    SectorOutOfRange,
    RequestTimedOut,
    RequestFailed(u8),
}

#[derive(Debug, Clone, Copy)]
pub struct BlockInfo {
    pub pci_bus: u8,
    pub pci_device: u8,
    pub pci_function: u8,
    pub io_base: u16,
    pub queue_size: u16,
    pub capacity_sectors: u64,
    pub flush_supported: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDescriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioBlockRequestHeader {
    request_type: u32,
    reserved: u32,
    sector: u64,
}

pub struct VirtioBlock {
    info: BlockInfo,
    queue_virt: *mut u8,
    used_offset: usize,
    request_phys: u64,
    request_virt: *mut u8,
    available_index: u16,
    used_index: u16,
}

impl VirtioBlock {
    pub fn discover() -> Result<Self, BlockError> {
        let function = find_legacy_virtio_block().ok_or(BlockError::DeviceNotFound)?;
        let bar0 = hardware::pci_read_u32(function, 0x10);
        if bar0 & 1 == 0 {
            return Err(BlockError::InvalidIoBar);
        }
        let io_base_raw = bar0 & !3;
        let io_base = u16::try_from(io_base_raw).map_err(|_| BlockError::InvalidIoBar)?;

        hardware::enable_io_bus_master(function);

        unsafe {
            outb(io_base + VIRTIO_DEVICE_STATUS, 0);
            outb(
                io_base + VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
            );
        }
        let host_features = unsafe { inl(io_base + VIRTIO_HOST_FEATURES) };
        let guest_features = host_features & VIRTIO_BLK_F_FLUSH;
        unsafe {
            outl(io_base + VIRTIO_GUEST_FEATURES, guest_features);
            outw(io_base + VIRTIO_QUEUE_SELECT, 0);
        }
        let queue_size = unsafe { inw(io_base + VIRTIO_QUEUE_SIZE) };
        if queue_size == 0 {
            return Err(BlockError::QueueUnavailable);
        }
        if queue_size < 3 {
            return Err(BlockError::QueueTooSmall);
        }

        let (queue_bytes, used_offset) = virtqueue_layout(queue_size);
        let queue_pages = queue_bytes.div_ceil(bootinfo::PAGE_SIZE as usize) as u64;
        let queue_phys = memory::allocate_contiguous_pages(queue_pages)
            .ok_or(BlockError::DmaAddressUnavailable)?;
        let queue_pfn =
            u32::try_from(queue_phys >> 12).map_err(|_| BlockError::QueueAddressTooHigh)?;
        let queue_virt = vm::physical_to_high_half(queue_phys)
            .map(|address| address as *mut u8)
            .ok_or(BlockError::DmaAddressUnavailable)?;
        unsafe {
            core::ptr::write_bytes(queue_virt, 0, queue_pages as usize * 4096);
            outl(io_base + VIRTIO_QUEUE_PFN, queue_pfn);
        }

        let request_phys = memory::allocate_page().ok_or(BlockError::DmaAddressUnavailable)?;
        let request_virt = vm::physical_to_high_half(request_phys)
            .map(|address| address as *mut u8)
            .ok_or(BlockError::DmaAddressUnavailable)?;
        unsafe {
            core::ptr::write_bytes(request_virt, 0, bootinfo::PAGE_SIZE as usize);
        }

        let capacity_sectors = unsafe {
            u64::from(inl(io_base + VIRTIO_DEVICE_CONFIG))
                | (u64::from(inl(io_base + VIRTIO_DEVICE_CONFIG + 4)) << 32)
        };
        unsafe {
            outb(
                io_base + VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
            );
        }
        let final_status = unsafe { inb(io_base + VIRTIO_DEVICE_STATUS) };
        if final_status & VIRTIO_STATUS_FAILED != 0 || final_status & VIRTIO_STATUS_DRIVER_OK == 0 {
            return Err(BlockError::DeviceRejected);
        }

        Ok(Self {
            info: BlockInfo {
                pci_bus: function.bus,
                pci_device: function.device,
                pci_function: function.function,
                io_base,
                queue_size,
                capacity_sectors,
                flush_supported: guest_features & VIRTIO_BLK_F_FLUSH != 0,
            },
            queue_virt,
            used_offset,
            request_phys,
            request_virt,
            available_index: 0,
            used_index: 0,
        })
    }

    pub const fn info(&self) -> BlockInfo {
        self.info
    }

    fn request(
        &mut self,
        request_type: u32,
        sector: u64,
        data: Option<&mut [u8; SECTOR_SIZE]>,
    ) -> Result<(), BlockError> {
        if request_type != VIRTIO_BLK_T_FLUSH && sector >= self.info.capacity_sectors {
            return Err(BlockError::SectorOutOfRange);
        }
        let header = VirtioBlockRequestHeader {
            request_type,
            reserved: 0,
            sector,
        };
        unsafe {
            core::ptr::write_volatile(
                self.request_virt
                    .add(REQUEST_HEADER_OFFSET as usize)
                    .cast::<VirtioBlockRequestHeader>(),
                header,
            );
            core::ptr::write_volatile(self.request_virt.add(REQUEST_STATUS_OFFSET as usize), 0xff);
        }

        let descriptor_count = if request_type == VIRTIO_BLK_T_FLUSH {
            self.write_descriptor(
                0,
                VirtqDescriptor {
                    address: self.request_phys + REQUEST_HEADER_OFFSET,
                    length: core::mem::size_of::<VirtioBlockRequestHeader>() as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: 1,
                },
            );
            self.write_descriptor(
                1,
                VirtqDescriptor {
                    address: self.request_phys + REQUEST_STATUS_OFFSET,
                    length: 1,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );
            2
        } else {
            if request_type == VIRTIO_BLK_T_OUT {
                let input = data.as_ref().ok_or(BlockError::DmaAddressUnavailable)?;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        input.as_ptr(),
                        self.request_virt.add(REQUEST_DATA_OFFSET as usize),
                        SECTOR_SIZE,
                    );
                }
            }
            self.write_descriptor(
                0,
                VirtqDescriptor {
                    address: self.request_phys + REQUEST_HEADER_OFFSET,
                    length: core::mem::size_of::<VirtioBlockRequestHeader>() as u32,
                    flags: VIRTQ_DESC_F_NEXT,
                    next: 1,
                },
            );
            self.write_descriptor(
                1,
                VirtqDescriptor {
                    address: self.request_phys + REQUEST_DATA_OFFSET,
                    length: SECTOR_SIZE as u32,
                    flags: VIRTQ_DESC_F_NEXT
                        | if request_type == VIRTIO_BLK_T_IN {
                            VIRTQ_DESC_F_WRITE
                        } else {
                            0
                        },
                    next: 2,
                },
            );
            self.write_descriptor(
                2,
                VirtqDescriptor {
                    address: self.request_phys + REQUEST_STATUS_OFFSET,
                    length: 1,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );
            3
        };
        debug_assert!(descriptor_count <= self.info.queue_size);

        let descriptor_bytes =
            core::mem::size_of::<VirtqDescriptor>() * usize::from(self.info.queue_size);
        let available = unsafe { self.queue_virt.add(descriptor_bytes) };
        let ring_slot = usize::from(self.available_index % self.info.queue_size);
        unsafe {
            core::ptr::write_volatile(available.add(4 + ring_slot * 2).cast::<u16>(), 0);
        }
        fence(Ordering::Release);
        self.available_index = self.available_index.wrapping_add(1);
        unsafe {
            core::ptr::write_volatile(available.add(2).cast::<u16>(), self.available_index);
            outw(self.info.io_base + VIRTIO_QUEUE_NOTIFY, 0);
        }

        let used = unsafe { self.queue_virt.add(self.used_offset) };
        let mut completed = false;
        let expected_used_index = self.used_index.wrapping_add(1);
        for spin in 0..REQUEST_TIMEOUT_SPINS {
            fence(Ordering::Acquire);
            let device_used_index = unsafe { core::ptr::read_volatile(used.add(2).cast::<u16>()) };
            if device_used_index != self.used_index {
                if device_used_index != expected_used_index {
                    return Err(BlockError::DeviceRejected);
                }
                let used_slot = usize::from(self.used_index % self.info.queue_size);
                let completed_descriptor =
                    unsafe { core::ptr::read_volatile(used.add(4 + used_slot * 8).cast::<u32>()) };
                if completed_descriptor != 0 {
                    return Err(BlockError::DeviceRejected);
                }
                self.used_index = device_used_index;
                completed = true;
                break;
            }
            if spin & 0x3fff == 0 {
                unsafe {
                    let _ = inb(self.info.io_base + VIRTIO_ISR_STATUS);
                }
            }
            core::hint::spin_loop();
        }
        if !completed {
            return Err(BlockError::RequestTimedOut);
        }
        fence(Ordering::Acquire);
        let status = unsafe {
            core::ptr::read_volatile(self.request_virt.add(REQUEST_STATUS_OFFSET as usize))
        };
        if status != 0 {
            return Err(BlockError::RequestFailed(status));
        }
        if request_type == VIRTIO_BLK_T_IN {
            let data = data.ok_or(BlockError::DmaAddressUnavailable)?;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.request_virt.add(REQUEST_DATA_OFFSET as usize),
                    data.as_mut_ptr(),
                    SECTOR_SIZE,
                );
            }
        }
        Ok(())
    }

    fn write_descriptor(&mut self, index: usize, descriptor: VirtqDescriptor) {
        let table = self.queue_virt.cast::<VirtqDescriptor>();
        unsafe {
            core::ptr::write_volatile(table.add(index), descriptor);
        }
    }
}

impl BlockDevice for VirtioBlock {
    fn sector_count(&self) -> u64 {
        self.info.capacity_sectors
    }

    fn supports_flush(&self) -> bool {
        self.info.flush_supported
    }

    fn read_sector(
        &mut self,
        sector: u64,
        output: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlockError> {
        self.request(VIRTIO_BLK_T_IN, sector, Some(output))
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; SECTOR_SIZE]) -> Result<(), BlockError> {
        let mut buffer = *input;
        self.request(VIRTIO_BLK_T_OUT, sector, Some(&mut buffer))
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        if !self.info.flush_supported {
            return Err(BlockError::DeviceRejected);
        }
        self.request(VIRTIO_BLK_T_FLUSH, 0, None)
    }
}

fn virtqueue_layout(queue_size: u16) -> (usize, usize) {
    let descriptor_bytes = core::mem::size_of::<VirtqDescriptor>() * usize::from(queue_size);
    let available_bytes = 6 + 2 * usize::from(queue_size);
    let used_offset = align_up(descriptor_bytes + available_bytes, 4096);
    let used_bytes = 6 + 8 * usize::from(queue_size);
    (used_offset + used_bytes, used_offset)
}

fn find_legacy_virtio_block() -> Option<hardware::PciAddress> {
    hardware::find_legacy_virtio_device(hardware::VIRTIO_BLOCK_LEGACY_DEVICE_ID)
        .map(|device| device.address)
}

const fn align_up(value: usize, alignment: usize) -> usize {
    value.saturating_add(alignment - 1) & !(alignment - 1)
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn outw(port: u16, value: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_queue_layout_obeys_device_alignment() {
        let (bytes, used_offset) = virtqueue_layout(128);
        assert_eq!(used_offset, 4096);
        assert!(bytes > 4096 && bytes <= 8192);
    }

    #[test]
    fn request_dma_offsets_fit_in_one_page() {
        assert_eq!(REQUEST_HEADER_OFFSET, 0);
        const {
            assert!(REQUEST_STATUS_OFFSET < REQUEST_DATA_OFFSET);
            assert!(REQUEST_DATA_OFFSET + SECTOR_SIZE as u64 <= bootinfo::PAGE_SIZE);
        }
    }
}
