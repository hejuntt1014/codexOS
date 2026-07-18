#![no_std]
#![no_main]

use bootinfo::BootInfo;
use core::panic::PanicInfo;
use kernel::input::{Ps2Event, Ps2InputDevices};
use kernel::{DesktopApp, interrupts};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KernelImageDescriptor {
    pub magic: [u8; 8],
    pub abi_version: u32,
    pub reserved: u32,
    pub entry_hint: u64,
}

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_DESCRIPTOR: KernelImageDescriptor = KernelImageDescriptor {
    magic: *b"CDXKERN\0",
    abi_version: 2,
    reserved: 0,
    entry_hint: 0,
};

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_BANNER: [u8; 23] = *b"codexOS kernel image v2";

#[cfg(target_os = "none")]
const LARGE_FILE_PROOF_PATH: &str = "/system/large-proof.bin";
#[cfg(target_os = "none")]
const LARGE_FILE_PROOF_BYTES: usize = 96 * 1024;
#[cfg(target_os = "none")]
static LARGE_FILE_PROOF: [u8; LARGE_FILE_PROOF_BYTES] = build_large_file_proof();

#[unsafe(no_mangle)]
/// Enters the resident kernel after the loader has exited UEFI boot services.
///
/// # Safety
///
/// `boot_info` must point to an initialized `BootInfo` that remains valid for
/// the lifetime of the kernel. The active page tables must contain the kernel
/// image mappings and the higher-half aliases described by that structure.
pub unsafe extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    kernel::serial::init();
    let Some(boot_info) = (unsafe { boot_info.as_ref() }) else {
        kernel::serial_println!("standalone boot info missing");
        interrupts::halt();
    };
    kernel::serial_println!("standalone boot info present");

    let root = match kernel::init_standalone(boot_info) {
        Ok(root) => root,
        Err(error) => {
            kernel::serial_println!("standalone initialization failed: {:?}", error);
            interrupts::halt();
        }
    };
    kernel::serial_println!("standalone runtime root=0x{:016x}", root);

    let mut desktop = DesktopApp::new(boot_info);
    desktop.note_handoff_complete();
    desktop.render(boot_info);
    kernel::serial_println!("standalone desktop rendered");

    #[cfg(target_os = "none")]
    {
        let isolation = match kernel::process::run_isolation_probe() {
            Ok(report) => report,
            Err(error) => {
                kernel::serial_println!("process isolation verification failed: {:?}", error);
                interrupts::halt();
            }
        };
        kernel::serial_println!(
            "process isolation verified: pid={} abi={} user-root=0x{:016x} syscalls={} denied=0x{:016x} pf=0x{:x} supervisor={} code-rx={} stack-rw-nx={}",
            isolation.process_id,
            isolation.syscall_abi_version,
            isolation.user_root,
            isolation.syscall_count,
            isolation.denied_address,
            isolation.page_fault_error,
            isolation.kernel_mapping_supervisor_only,
            isolation.code_read_execute,
            isolation.stack_read_write_no_execute
        );
    }

    #[cfg(target_os = "none")]
    let hardware_summary = {
        let inventory = kernel::hardware::scan_pci();
        let summary = inventory.summary();
        let Some(block_address) = summary.virtio_block else {
            kernel::serial_println!(
                "hardware inventory failed: pci-devices={} virtio-blk=false virtio-net={}",
                summary.pci_devices,
                summary.virtio_net.is_some()
            );
            interrupts::halt();
        };
        let Some(network_address) = summary.virtio_net else {
            kernel::serial_println!(
                "hardware inventory failed: pci-devices={} virtio-blk=true virtio-net=false",
                summary.pci_devices
            );
            interrupts::halt();
        };
        kernel::serial_println!(
            "hardware inventory: pci-devices={} overflow={} bridges={} storage={} network={} display={} usb={} input={} io-bars={} virtio-legacy={} virtio-blk={:02x}:{:02x}.{} virtio-net={:02x}:{:02x}.{}",
            summary.pci_devices,
            summary.overflow,
            summary.bridge_devices,
            summary.storage_controllers,
            summary.network_controllers,
            summary.display_controllers,
            summary.usb_controllers,
            summary.input_controllers,
            summary.io_bar_devices,
            summary.virtio_legacy_devices,
            block_address.bus,
            block_address.device,
            block_address.function,
            network_address.bus,
            network_address.device,
            network_address.function
        );
        summary
    };

    #[cfg(target_os = "none")]
    let mut filesystem = {
        let block = match kernel::block::VirtioBlock::discover() {
            Ok(device) => device,
            Err(error) => {
                kernel::serial_println!("virtio-blk initialization failed: {:?}", error);
                interrupts::halt();
            }
        };
        let block_info = block.info();
        let discovered_block = kernel::hardware::PciAddress::new(
            block_info.pci_bus,
            block_info.pci_device,
            block_info.pci_function,
        );
        if hardware_summary.virtio_block != Some(discovered_block) {
            kernel::serial_println!(
                "hardware driver binding failed: device=virtio-blk inventory-match=false"
            );
            interrupts::halt();
        }
        kernel::serial_println!(
            "virtio-blk: pci={:02x}:{:02x}.{} io=0x{:04x} queue={} sectors={} capacity={} MiB flush={}",
            block_info.pci_bus,
            block_info.pci_device,
            block_info.pci_function,
            block_info.io_base,
            block_info.queue_size,
            block_info.capacity_sectors,
            block_info.capacity_sectors / 2048,
            block_info.flush_supported
        );
        kernel::serial_println!(
            "hardware driver binding: device=virtio-blk pci={:02x}:{:02x}.{} driver=ready inventory-match=true",
            block_info.pci_bus,
            block_info.pci_device,
            block_info.pci_function
        );

        let mut filesystem = match kernel::fs::CodexFs::mount_or_format(block) {
            Ok(filesystem) => filesystem,
            Err(error) => {
                kernel::serial_println!("codexfs mount failed: {:?}", error);
                interrupts::halt();
            }
        };
        let mount = filesystem.info();
        kernel::serial_println!(
            "codexfs: mounted state={} generation={} files={} directories={} sectors={} record-slots={} slot-sectors={} active-record={} active-sectors={} max-record-bytes={}",
            mount.mount_state.as_str(),
            mount.generation,
            mount.file_count,
            mount.directory_count,
            mount.capacity_sectors,
            mount.record_slots,
            mount.record_slot_sectors,
            mount.active_record_start,
            mount.active_record_sectors,
            mount.max_record_bytes
        );

        const BOOT_COUNT_PATH: &str = "/system/boot-count";
        if let Err(error) = filesystem.create_dir_all("/system", 0o755) {
            kernel::serial_println!("codexfs namespace initialization failed: {:?}", error);
            interrupts::halt();
        }
        if let Err(error) = filesystem.create_dir_all("/system/bin", 0o755) {
            kernel::serial_println!(
                "codexfs binary directory initialization failed: {:?}",
                error
            );
            interrupts::halt();
        }
        let previous = match filesystem.read_file(BOOT_COUNT_PATH) {
            None => 0,
            Some(value) => match parse_decimal_u64(value) {
                Some(value) => value,
                None => {
                    kernel::serial_println!("codexfs boot counter contains invalid data");
                    interrupts::halt();
                }
            },
        };
        let Some(current) = previous.checked_add(1) else {
            kernel::serial_println!("codexfs boot counter exhausted");
            interrupts::halt();
        };
        let mut decimal_buffer = [0_u8; 20];
        let encoded = encode_decimal_u64(current, &mut decimal_buffer);
        if let Err(error) = filesystem.write_file_with_permissions(BOOT_COUNT_PATH, encoded, 0o600)
        {
            kernel::serial_println!("codexfs commit failed: {:?}", error);
            interrupts::halt();
        }
        if let Err(error) = filesystem.verify_committed_state() {
            kernel::serial_println!("codexfs committed-state verification failed: {:?}", error);
            interrupts::halt();
        }
        if filesystem
            .read_file(BOOT_COUNT_PATH)
            .and_then(parse_decimal_u64)
            != Some(current)
        {
            kernel::serial_println!("codexfs boot counter readback mismatch");
            interrupts::halt();
        }
        let large_file_written = match filesystem.read_file(LARGE_FILE_PROOF_PATH) {
            Some(bytes) if bytes == LARGE_FILE_PROOF.as_slice() => false,
            _ => {
                if let Err(error) = filesystem.write_file_with_permissions(
                    LARGE_FILE_PROOF_PATH,
                    LARGE_FILE_PROOF.as_slice(),
                    0o640,
                ) {
                    kernel::serial_println!("codexfs large-file commit failed: {:?}", error);
                    interrupts::halt();
                }
                true
            }
        };
        if let Err(error) = filesystem.verify_committed_state() {
            kernel::serial_println!("codexfs large-file verification failed: {:?}", error);
            interrupts::halt();
        }
        if filesystem.read_file(LARGE_FILE_PROOF_PATH) != Some(LARGE_FILE_PROOF.as_slice()) {
            kernel::serial_println!("codexfs large-file readback mismatch");
            interrupts::halt();
        }
        let large_file_info = filesystem.info();
        kernel::serial_println!(
            "filesystem large-file verified: path={} bytes={} checksum=0x{:08x} record-sectors={} slot-sectors={} active-record={} written={}",
            LARGE_FILE_PROOF_PATH,
            LARGE_FILE_PROOF_BYTES,
            checksum32(LARGE_FILE_PROOF.as_slice()),
            large_file_info.active_record_sectors,
            large_file_info.record_slot_sectors,
            large_file_info.active_record_start,
            large_file_written
        );
        kernel::serial_println!(
            "filesystem persistence: previous={} current={} generation={} directories={} verified=true",
            previous,
            current,
            filesystem.info().generation,
            filesystem.info().directory_count
        );
        filesystem
    };

    #[cfg(target_os = "none")]
    let mut network_interface = {
        let interface = match kernel::network::configure() {
            Ok(interface) => interface,
            Err(error) => {
                kernel::serial_println!("network configuration failed: {:?}", error);
                interrupts::halt();
            }
        };
        let network = interface.report();
        let discovered_network = kernel::hardware::PciAddress::new(
            network.pci_bus,
            network.pci_device,
            network.pci_function,
        );
        if hardware_summary.virtio_net != Some(discovered_network) {
            kernel::serial_println!(
                "hardware driver binding failed: device=virtio-net inventory-match=false"
            );
            interrupts::halt();
        }
        kernel::serial_println!(
            "virtio-net: pci={:02x}:{:02x}.{} io=0x{:04x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            network.pci_bus,
            network.pci_device,
            network.pci_function,
            network.io_base,
            network.mac[0],
            network.mac[1],
            network.mac[2],
            network.mac[3],
            network.mac[4],
            network.mac[5]
        );
        kernel::serial_println!(
            "hardware driver binding: device=virtio-net pci={:02x}:{:02x}.{} driver=ready inventory-match=true",
            network.pci_bus,
            network.pci_device,
            network.pci_function
        );
        kernel::serial_println!(
            "network configured: ipv4={}.{}.{}.{} mask={}.{}.{}.{} gateway={}.{}.{}.{} dhcp={}.{}.{}.{} dns={}.{}.{}.{} lease={}s tx={} rx={}",
            network.address[0],
            network.address[1],
            network.address[2],
            network.address[3],
            network.subnet_mask[0],
            network.subnet_mask[1],
            network.subnet_mask[2],
            network.subnet_mask[3],
            network.gateway[0],
            network.gateway[1],
            network.gateway[2],
            network.gateway[3],
            network.dhcp_server[0],
            network.dhcp_server[1],
            network.dhcp_server[2],
            network.dhcp_server[3],
            network.dns_server[0],
            network.dns_server[1],
            network.dns_server[2],
            network.dns_server[3],
            network.lease_seconds,
            network.transmitted_frames,
            network.received_frames
        );
        kernel::serial_println!(
            "arp gateway verified: ip={}.{}.{}.{} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            network.gateway[0],
            network.gateway[1],
            network.gateway[2],
            network.gateway[3],
            network.gateway_mac[0],
            network.gateway_mac[1],
            network.gateway_mac[2],
            network.gateway_mac[3],
            network.gateway_mac[4],
            network.gateway_mac[5]
        );
        kernel::serial_println!(
            "icmp echo verified: destination={}.{}.{}.{} sequence={} tx={} rx={}",
            network.gateway[0],
            network.gateway[1],
            network.gateway[2],
            network.gateway[3],
            network.icmp_echo_sequence,
            network.transmitted_frames,
            network.received_frames
        );
        kernel::serial_println!(
            "dns resolved: name=example.com server={}.{}.{}.{} answer={}.{}.{}.{} query-id=0x{:04x} tx={} rx={}",
            network.dns_server[0],
            network.dns_server[1],
            network.dns_server[2],
            network.dns_server[3],
            network.dns_answer[0],
            network.dns_answer[1],
            network.dns_answer[2],
            network.dns_answer[3],
            network.dns_query_id,
            network.transmitted_frames,
            network.received_frames
        );
        kernel::serial_println!(
            "tcp http verified: host=example.com remote={}.{}.{}.{} port=80 status={} bytes={} source-port={} tx={} rx={}",
            network.dns_answer[0],
            network.dns_answer[1],
            network.dns_answer[2],
            network.dns_answer[3],
            network.http_status,
            network.http_response_bytes,
            network.tcp_source_port,
            network.transmitted_frames,
            network.received_frames
        );
        kernel::serial_println!(
            "network tcp listener ready: port={} protocol=http capacity={} idle-timeout-ticks={} tx={} rx={}",
            kernel::network::TCP_LISTENER_PORT,
            kernel::network::TCP_LISTENER_CAPACITY,
            kernel::network::TCP_LISTENER_IDLE_TIMEOUT_TICKS,
            network.transmitted_frames,
            network.received_frames
        );
        interface
    };

    if interrupts::activate_hardware() {
        kernel::serial_println!("standalone timer interrupts active");
    } else {
        kernel::serial_println!("standalone timer interrupt activation failed");
        interrupts::halt();
    }

    #[cfg(target_os = "none")]
    {
        let scheduler = match kernel::scheduler::run_preemption_gate() {
            Ok(report) => report,
            Err(error) => {
                kernel::serial_println!("scheduler verification failed: {:?}", error);
                interrupts::halt();
            }
        };
        kernel::serial_println!(
            "scheduler verified: processes={} ticks={} timer-preemptions={} switches={} min-dispatches={} idle-halts={} reclaimed-pages={} fault-pid={} fault=0x{:016x} err=0x{:x}",
            scheduler.process_count,
            scheduler.timer_ticks,
            scheduler.timer_preemptions,
            scheduler.context_switches,
            scheduler.minimum_dispatches,
            scheduler.idle_halts,
            scheduler.reclaimed_pages,
            scheduler.faulted_pid,
            scheduler.fault_address,
            scheduler.fault_error
        );

        let persistent = match kernel::scheduler::run_persistent_executable_gate(&mut filesystem) {
            Ok(report) => report,
            Err(error) => {
                kernel::serial_println!("persistent executable verification failed: {:?}", error);
                interrupts::halt();
            }
        };
        kernel::serial_println!(
            "persistent executable verified: path={} bytes={} entry=0x{:016x} segments={} exit={} ticks={} idle-halts={} reclaimed-pages={} generation={} installed={} sha256-prefix={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            persistent.path,
            persistent.bytes,
            persistent.entry,
            persistent.load_segments,
            persistent.exit_status,
            persistent.timer_ticks,
            persistent.idle_halts,
            persistent.reclaimed_pages,
            persistent.generation,
            persistent.installed,
            persistent.sha256[0],
            persistent.sha256[1],
            persistent.sha256[2],
            persistent.sha256[3],
            persistent.sha256[4],
            persistent.sha256[5],
            persistent.sha256[6],
            persistent.sha256[7]
        );
    }

    let mut input_devices = Ps2InputDevices::initialize();
    let pointer = input_devices.pointer_status();
    kernel::serial_println!("standalone keyboard polling active");
    match pointer.device_id {
        Some(device_id) => kernel::serial_println!(
            "standalone pointer polling active: device=ps2 enabled={} id=0x{:02x} acknowledgements={}",
            pointer.enabled,
            device_id,
            pointer.acknowledgements
        ),
        None => kernel::serial_println!(
            "standalone pointer polling active: device=ps2 enabled={} id=none acknowledgements={}",
            pointer.enabled,
            pointer.acknowledgements
        ),
    }
    let mut pointer_event_reports = 0_u8;
    loop {
        let mut received_input = false;
        while let Some(event) = input_devices.poll_event() {
            match event {
                Ps2Event::Keyboard(input) => desktop.handle_input(input),
                Ps2Event::Pointer(sample) => {
                    desktop.handle_pointer(sample);
                    if pointer_event_reports < 4 {
                        kernel::serial_println!(
                            "pointer input event: device=ps2 dx={} dy={} left={} right={}",
                            sample.delta_x,
                            sample.delta_y,
                            sample.left_button,
                            sample.right_button
                        );
                        pointer_event_reports += 1;
                    }
                }
            }
            received_input = true;
        }
        #[cfg(target_os = "none")]
        match network_interface.poll() {
            Ok(result) => {
                received_input |= result.received_frame;
                if result.expired_tcp_connections != 0 {
                    kernel::serial_println!(
                        "tcp listener expired: count={} idle-timeout-ticks={}",
                        result.expired_tcp_connections,
                        kernel::network::TCP_LISTENER_IDLE_TIMEOUT_TICKS
                    );
                }
                if result.closed_tcp_connections != 0 {
                    kernel::serial_println!(
                        "tcp listener closed: count={} total={}",
                        result.closed_tcp_connections,
                        result.total_closed_tcp_connections
                    );
                }
                if let Some(event) = result.tcp_server {
                    let network = network_interface.report();
                    match event.kind {
                        kernel::network::TcpServerEventKind::Accepted => {
                            kernel::serial_println!(
                                "tcp listener accepted: port={} remote={}.{}.{}.{}:{} tx={} rx={}",
                                event.local_port,
                                event.remote_address[0],
                                event.remote_address[1],
                                event.remote_address[2],
                                event.remote_address[3],
                                event.remote_port,
                                network.transmitted_frames,
                                network.received_frames
                            );
                        }
                        kernel::network::TcpServerEventKind::Served => {
                            kernel::serial_println!(
                                "tcp listener served: port={} remote={}.{}.{}.{}:{} request-bytes={} response-bytes={} connections={} tx={} rx={}",
                                event.local_port,
                                event.remote_address[0],
                                event.remote_address[1],
                                event.remote_address[2],
                                event.remote_address[3],
                                event.remote_port,
                                event.request_bytes,
                                event.response_bytes,
                                event.connection_count,
                                network.transmitted_frames,
                                network.received_frames
                            );
                        }
                    }
                }
            }
            Err(error) => {
                kernel::serial_println!("network runtime failed: {:?}", error);
                interrupts::halt();
            }
        }

        if desktop.needs_redraw() {
            desktop.render(boot_info);
        }
        if desktop.should_exit() {
            kernel::serial_println!("standalone desktop requested shutdown");
            interrupts::halt();
        }

        if !received_input {
            interrupts::wait_for_interrupt();
        }
    }
}

#[cfg(target_os = "none")]
fn parse_decimal_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_u64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))?;
    }
    Some(value)
}

#[cfg(target_os = "none")]
fn encode_decimal_u64(mut value: u64, buffer: &mut [u8; 20]) -> &[u8] {
    let mut offset = buffer.len();
    loop {
        offset -= 1;
        buffer[offset] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            return &buffer[offset..];
        }
    }
}

#[cfg(target_os = "none")]
const fn build_large_file_proof() -> [u8; LARGE_FILE_PROOF_BYTES] {
    let mut bytes = [0_u8; LARGE_FILE_PROOF_BYTES];
    let mut index = 0;
    while index < LARGE_FILE_PROOF_BYTES {
        bytes[index] = ((index * 31 + index / 251) & 0xff) as u8;
        index += 1;
    }
    bytes
}

#[cfg(target_os = "none")]
fn checksum32(bytes: &[u8]) -> u32 {
    let mut checksum = 0_u32;
    for byte in bytes {
        checksum = checksum.rotate_left(5) ^ u32::from(*byte);
    }
    checksum
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    kernel::serial::init();
    kernel::serial_println!("[PANIC] {}", info);
    interrupts::halt()
}
