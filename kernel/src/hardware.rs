const PCI_CONFIG_ADDRESS: u16 = 0xcf8;
const PCI_CONFIG_DATA: u16 = 0xcfc;
const PCI_VENDOR_NONE: u16 = 0xffff;
const PCI_HEADER_MULTIFUNCTION: u8 = 0x80;
const PCI_CLASS_MASS_STORAGE: u8 = 0x01;
const PCI_CLASS_NETWORK: u8 = 0x02;
const PCI_CLASS_DISPLAY: u8 = 0x03;
const PCI_CLASS_BRIDGE: u8 = 0x06;
const PCI_CLASS_INPUT: u8 = 0x09;
const PCI_CLASS_SERIAL_BUS: u8 = 0x0c;
const PCI_SUBCLASS_BRIDGE_HOST: u8 = 0x00;
const PCI_SUBCLASS_BRIDGE_ISA: u8 = 0x01;
const PCI_SUBCLASS_BRIDGE_PCI: u8 = 0x04;
const PCI_SUBCLASS_USB: u8 = 0x03;
const PCI_BAR0: u8 = 0x10;
const PCI_BAR_COUNT: usize = 6;
const PCI_COMMAND: u8 = 0x04;
const PCI_COMMAND_IO_SPACE: u16 = 0x0001;
const PCI_COMMAND_BUS_MASTER: u16 = 0x0004;

pub const MAX_PCI_DEVICES: usize = 64;
pub const VIRTIO_VENDOR_ID: u16 = 0x1af4;
pub const VIRTIO_NET_LEGACY_DEVICE_ID: u16 = 0x1000;
pub const VIRTIO_BLOCK_LEGACY_DEVICE_ID: u16 = 0x1001;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            bus,
            device,
            function,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDevice {
    pub address: PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
    pub bars: [u32; PCI_BAR_COUNT],
}

impl PciDevice {
    pub fn role(&self) -> PciDeviceRole {
        classify_pci_device(
            self.vendor_id,
            self.device_id,
            self.class_code,
            self.subclass,
            self.prog_if,
        )
    }

    pub fn io_bar(&self, index: usize) -> Option<u16> {
        if index >= self.bars.len() {
            return None;
        }
        let raw = self.bars[index];
        if raw & 1 == 0 {
            return None;
        }
        u16::try_from(raw & !3).ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciDeviceRole {
    VirtioNet,
    VirtioBlock,
    VirtioOther,
    StorageController,
    NetworkController,
    DisplayController,
    HostBridge,
    IsaBridge,
    PciBridge,
    UsbController,
    InputController,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardwareSummary {
    pub pci_devices: usize,
    pub overflow: bool,
    pub bridge_devices: usize,
    pub storage_controllers: usize,
    pub network_controllers: usize,
    pub display_controllers: usize,
    pub usb_controllers: usize,
    pub input_controllers: usize,
    pub io_bar_devices: usize,
    pub virtio_legacy_devices: usize,
    pub virtio_block: Option<PciAddress>,
    pub virtio_net: Option<PciAddress>,
}

impl HardwareSummary {
    pub fn has_boot_storage_and_network(&self) -> bool {
        self.virtio_block.is_some() && self.virtio_net.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciInventory {
    devices: [Option<PciDevice>; MAX_PCI_DEVICES],
    count: usize,
    overflow: bool,
}

impl PciInventory {
    pub const fn empty() -> Self {
        Self {
            devices: [None; MAX_PCI_DEVICES],
            count: 0,
            overflow: false,
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    pub fn get(&self, index: usize) -> Option<PciDevice> {
        if index >= self.count {
            return None;
        }
        self.devices[index]
    }

    pub fn find_by_address(&self, address: PciAddress) -> Option<PciDevice> {
        let mut index = 0;
        while index < self.count {
            if let Some(device) = self.devices[index]
                && device.address == address
            {
                return Some(device);
            }
            index += 1;
        }
        None
    }

    pub fn summary(&self) -> HardwareSummary {
        let mut summary = HardwareSummary {
            pci_devices: self.count,
            overflow: self.overflow,
            bridge_devices: 0,
            storage_controllers: 0,
            network_controllers: 0,
            display_controllers: 0,
            usb_controllers: 0,
            input_controllers: 0,
            io_bar_devices: 0,
            virtio_legacy_devices: 0,
            virtio_block: None,
            virtio_net: None,
        };

        let mut index = 0;
        while index < self.count {
            if let Some(device) = self.devices[index] {
                if device.io_bar(0).is_some() {
                    summary.io_bar_devices += 1;
                }
                match device.role() {
                    PciDeviceRole::VirtioBlock => {
                        summary.storage_controllers += 1;
                        summary.virtio_legacy_devices += 1;
                        if summary.virtio_block.is_none() {
                            summary.virtio_block = Some(device.address);
                        }
                    }
                    PciDeviceRole::VirtioNet => {
                        summary.network_controllers += 1;
                        summary.virtio_legacy_devices += 1;
                        if summary.virtio_net.is_none() {
                            summary.virtio_net = Some(device.address);
                        }
                    }
                    PciDeviceRole::VirtioOther => {
                        summary.virtio_legacy_devices += 1;
                    }
                    PciDeviceRole::StorageController => {
                        summary.storage_controllers += 1;
                    }
                    PciDeviceRole::NetworkController => {
                        summary.network_controllers += 1;
                    }
                    PciDeviceRole::DisplayController => {
                        summary.display_controllers += 1;
                    }
                    PciDeviceRole::HostBridge
                    | PciDeviceRole::IsaBridge
                    | PciDeviceRole::PciBridge => {
                        summary.bridge_devices += 1;
                    }
                    PciDeviceRole::UsbController => {
                        summary.usb_controllers += 1;
                    }
                    PciDeviceRole::InputController => {
                        summary.input_controllers += 1;
                    }
                    PciDeviceRole::Other => {}
                }
            }
            index += 1;
        }

        summary
    }

    fn push(&mut self, device: PciDevice) {
        if self.count == self.devices.len() {
            self.overflow = true;
            return;
        }
        self.devices[self.count] = Some(device);
        self.count += 1;
    }
}

pub fn scan_pci() -> PciInventory {
    let mut inventory = PciInventory::empty();
    for bus in 0..=u8::MAX {
        for device in 0..32_u8 {
            let function0 = PciAddress::new(bus, device, 0);
            if pci_vendor_id(function0) == PCI_VENDOR_NONE {
                continue;
            }
            let header_type = pci_header_type(function0);
            let function_count = if header_type & PCI_HEADER_MULTIFUNCTION != 0 {
                8
            } else {
                1
            };
            for function in 0..function_count {
                let address = PciAddress::new(bus, device, function);
                if let Some(pci_device) = read_pci_device(address) {
                    inventory.push(pci_device);
                }
            }
        }
    }
    inventory
}

pub fn find_legacy_virtio_device(device_id: u16) -> Option<PciDevice> {
    for bus in 0..=u8::MAX {
        for device in 0..32_u8 {
            let function0 = PciAddress::new(bus, device, 0);
            if pci_vendor_id(function0) == PCI_VENDOR_NONE {
                continue;
            }
            let header_type = pci_header_type(function0);
            let function_count = if header_type & PCI_HEADER_MULTIFUNCTION != 0 {
                8
            } else {
                1
            };
            for function in 0..function_count {
                let address = PciAddress::new(bus, device, function);
                let id = pci_read_u32(address, 0x00);
                if id as u16 == VIRTIO_VENDOR_ID && (id >> 16) as u16 == device_id {
                    return read_pci_device(address);
                }
            }
        }
    }
    None
}

pub fn enable_io_bus_master(address: PciAddress) -> u16 {
    let command = pci_read_u32(address, PCI_COMMAND) as u16;
    let enabled = command | PCI_COMMAND_IO_SPACE | PCI_COMMAND_BUS_MASTER;
    pci_write_u16(address, PCI_COMMAND, enabled);
    enabled
}

pub fn pci_read_u32(address: PciAddress, offset: u8) -> u32 {
    let config_address = pci_config_address(address, offset);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, config_address);
        inl(PCI_CONFIG_DATA)
    }
}

pub fn pci_write_u16(address: PciAddress, offset: u8, value: u16) {
    let config_address = pci_config_address(address, offset);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, config_address);
        outw(PCI_CONFIG_DATA + u16::from(offset & 2), value);
    }
}

pub fn classify_pci_device(
    vendor_id: u16,
    device_id: u16,
    class_code: u8,
    subclass: u8,
    _prog_if: u8,
) -> PciDeviceRole {
    if vendor_id == VIRTIO_VENDOR_ID {
        return match device_id {
            VIRTIO_NET_LEGACY_DEVICE_ID => PciDeviceRole::VirtioNet,
            VIRTIO_BLOCK_LEGACY_DEVICE_ID => PciDeviceRole::VirtioBlock,
            0x1002..=0x103f => PciDeviceRole::VirtioOther,
            _ => PciDeviceRole::Other,
        };
    }

    match (class_code, subclass) {
        (PCI_CLASS_MASS_STORAGE, _) => PciDeviceRole::StorageController,
        (PCI_CLASS_NETWORK, _) => PciDeviceRole::NetworkController,
        (PCI_CLASS_DISPLAY, _) => PciDeviceRole::DisplayController,
        (PCI_CLASS_BRIDGE, PCI_SUBCLASS_BRIDGE_HOST) => PciDeviceRole::HostBridge,
        (PCI_CLASS_BRIDGE, PCI_SUBCLASS_BRIDGE_ISA) => PciDeviceRole::IsaBridge,
        (PCI_CLASS_BRIDGE, PCI_SUBCLASS_BRIDGE_PCI) => PciDeviceRole::PciBridge,
        (PCI_CLASS_SERIAL_BUS, PCI_SUBCLASS_USB) => PciDeviceRole::UsbController,
        (PCI_CLASS_INPUT, _) => PciDeviceRole::InputController,
        _ => PciDeviceRole::Other,
    }
}

fn read_pci_device(address: PciAddress) -> Option<PciDevice> {
    let id = pci_read_u32(address, 0x00);
    let vendor_id = id as u16;
    if vendor_id == PCI_VENDOR_NONE {
        return None;
    }
    let revision_class = pci_read_u32(address, 0x08);
    let header = pci_read_u32(address, 0x0c);
    let mut bars = [0_u32; PCI_BAR_COUNT];
    let mut index = 0;
    while index < PCI_BAR_COUNT {
        bars[index] = pci_read_u32(address, PCI_BAR0 + (index as u8) * 4);
        index += 1;
    }

    Some(PciDevice {
        address,
        vendor_id,
        device_id: (id >> 16) as u16,
        class_code: (revision_class >> 24) as u8,
        subclass: (revision_class >> 16) as u8,
        prog_if: (revision_class >> 8) as u8,
        revision: revision_class as u8,
        header_type: ((header >> 16) & 0xff) as u8,
        bars,
    })
}

fn pci_vendor_id(address: PciAddress) -> u16 {
    pci_read_u32(address, 0x00) as u16
}

fn pci_header_type(address: PciAddress) -> u8 {
    ((pci_read_u32(address, 0x0c) >> 16) & 0xff) as u8
}

fn pci_config_address(address: PciAddress, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(address.bus) << 16)
        | (u32::from(address.device) << 11)
        | (u32::from(address.function) << 8)
        | u32::from(offset & 0xfc)
}

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

unsafe fn outw(port: u16, value: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
    }
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

    fn device(
        vendor_id: u16,
        device_id: u16,
        class_code: u8,
        subclass: u8,
        bars: [u32; PCI_BAR_COUNT],
    ) -> PciDevice {
        PciDevice {
            address: PciAddress::new(0, (device_id & 0x1f) as u8, 0),
            vendor_id,
            device_id,
            class_code,
            subclass,
            prog_if: 0,
            revision: 0,
            header_type: 0,
            bars,
        }
    }

    #[test]
    fn classifies_legacy_virtio_devices_before_generic_classes() {
        assert_eq!(
            classify_pci_device(VIRTIO_VENDOR_ID, VIRTIO_NET_LEGACY_DEVICE_ID, 0xff, 0, 0),
            PciDeviceRole::VirtioNet
        );
        assert_eq!(
            classify_pci_device(VIRTIO_VENDOR_ID, VIRTIO_BLOCK_LEGACY_DEVICE_ID, 0xff, 0, 0),
            PciDeviceRole::VirtioBlock
        );
        assert_eq!(
            classify_pci_device(0x8086, 0x100e, PCI_CLASS_NETWORK, 0, 0),
            PciDeviceRole::NetworkController
        );
    }

    #[test]
    fn inventory_summary_counts_required_boot_devices() {
        let mut inventory = PciInventory::empty();
        inventory.push(device(
            0x8086,
            0x29c0,
            PCI_CLASS_BRIDGE,
            PCI_SUBCLASS_BRIDGE_HOST,
            [0; PCI_BAR_COUNT],
        ));
        inventory.push(device(
            VIRTIO_VENDOR_ID,
            VIRTIO_BLOCK_LEGACY_DEVICE_ID,
            0xff,
            0,
            [0xc001, 0, 0, 0, 0, 0],
        ));
        inventory.push(device(
            VIRTIO_VENDOR_ID,
            VIRTIO_NET_LEGACY_DEVICE_ID,
            0xff,
            0,
            [0xc101, 0, 0, 0, 0, 0],
        ));

        let summary = inventory.summary();
        assert!(summary.has_boot_storage_and_network());
        assert_eq!(summary.pci_devices, 3);
        assert_eq!(summary.bridge_devices, 1);
        assert_eq!(summary.storage_controllers, 1);
        assert_eq!(summary.network_controllers, 1);
        assert_eq!(summary.io_bar_devices, 2);
        assert_eq!(summary.virtio_legacy_devices, 2);
        assert_eq!(summary.virtio_block, Some(PciAddress::new(0, 1, 0)));
        assert_eq!(summary.virtio_net, Some(PciAddress::new(0, 0, 0)));
    }

    #[test]
    fn inventory_records_overflow_without_overwriting_entries() {
        let mut inventory = PciInventory::empty();
        let sample = device(
            0x8086,
            0x1237,
            PCI_CLASS_BRIDGE,
            PCI_SUBCLASS_BRIDGE_HOST,
            [0; 6],
        );
        for _ in 0..MAX_PCI_DEVICES {
            inventory.push(sample);
        }
        inventory.push(device(0x1234, 0x5678, PCI_CLASS_DISPLAY, 0, [0; 6]));

        assert_eq!(inventory.count(), MAX_PCI_DEVICES);
        assert!(inventory.overflowed());
        assert_eq!(inventory.get(MAX_PCI_DEVICES), None);
    }
}
