use core::sync::atomic::{Ordering, fence};

use crate::{hardware, memory, vm};

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
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const RX_QUEUE_INDEX: u16 = 0;
const TX_QUEUE_INDEX: u16 = 1;
const VIRTIO_NET_HEADER_BYTES: usize = 10;
const DMA_BUFFER_BYTES: usize = bootinfo::PAGE_SIZE as usize;
const MAX_ETHERNET_FRAME: usize = 1514;
const MIN_ETHERNET_FRAME: usize = 60;
const DEVICE_TIMEOUT_SPINS: usize = 20_000_000;
const PROTOCOL_TIMEOUT_SPINS: usize = 100_000_000;
const DHCP_XID: u32 = 0x4344_584f;
const DHCP_PAYLOAD_BYTES: usize = 300;
const ETHERNET_HEADER_BYTES: usize = 14;
const IPV4_HEADER_BYTES: usize = 20;
const UDP_HEADER_BYTES: usize = 8;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const IP_PROTOCOL_ICMP: u8 = 1;
const IP_PROTOCOL_TCP: u8 = 6;
const IP_PROTOCOL_UDP: u8 = 17;
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DNS_SERVER_PORT: u16 = 53;
const DNS_CLIENT_PORT: u16 = 49152;
const DNS_QUERY_ID: u16 = 0x4344;
const DNS_QUERY_NAME: &str = "example.com";
const HTTP_CLIENT_PORT: u16 = 49153;
const HTTP_SERVER_PORT: u16 = 80;
pub const TCP_LISTENER_PORT: u16 = 8080;
pub const TCP_LISTENER_CAPACITY: usize = 8;
pub const TCP_LISTENER_IDLE_TIMEOUT_TICKS: u64 = 3_000;
const TCP_INITIAL_SEQUENCE: u32 = 0x4344_5801;
const TCP_SERVER_INITIAL_SEQUENCE: u32 = 0x4344_5901;
const TCP_FLAG_FIN: u16 = 0x01;
const TCP_FLAG_SYN: u16 = 0x02;
const TCP_FLAG_RST: u16 = 0x04;
const TCP_FLAG_PSH: u16 = 0x08;
const TCP_FLAG_ACK: u16 = 0x10;
const HTTP_REQUEST: &[u8] = b"GET / HTTP/1.0\r\nHost: example.com\r\nConnection: close\r\n\r\n";
const HTTP_LISTENER_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 24\r\nConnection: close\r\n\r\ncodexOS listener online\n";
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const ICMP_ECHO_IDENTIFIER: u16 = 0x4344;
const ICMP_ECHO_SEQUENCE: u16 = 1;
const ICMP_ECHO_PAYLOAD: &[u8; 16] = b"codexOS-net-echo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    DeviceNotFound,
    InvalidIoBar,
    QueueUnavailable(u16),
    QueueTooSmall(u16),
    QueueAddressTooHigh,
    DmaAddressUnavailable,
    DeviceRejected,
    FrameTooLarge,
    PacketMalformed,
    RequestTimedOut,
    DhcpTimedOut,
    DhcpRejected,
    ArpTimedOut,
    IcmpTimedOut,
    DnsTimedOut,
    DnsRejected,
    TcpTimedOut,
    TcpRejected,
    HttpRejected,
}

#[derive(Debug, Clone, Copy)]
pub struct NetworkReport {
    pub pci_bus: u8,
    pub pci_device: u8,
    pub pci_function: u8,
    pub io_base: u16,
    pub mac: [u8; 6],
    pub address: [u8; 4],
    pub subnet_mask: [u8; 4],
    pub gateway: [u8; 4],
    pub dhcp_server: [u8; 4],
    pub dns_server: [u8; 4],
    pub dns_answer: [u8; 4],
    pub dns_query_id: u16,
    pub http_status: u16,
    pub http_response_bytes: usize,
    pub tcp_source_port: u16,
    pub gateway_mac: [u8; 6],
    pub icmp_echo_sequence: u16,
    pub lease_seconds: u32,
    pub transmitted_frames: u64,
    pub received_frames: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpServerEventKind {
    Accepted,
    Served,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpServerEvent {
    pub kind: TcpServerEventKind,
    pub remote_address: [u8; 4],
    pub remote_port: u16,
    pub local_port: u16,
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub connection_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkPollResult {
    pub received_frame: bool,
    pub tcp_server: Option<TcpServerEvent>,
    pub expired_tcp_connections: usize,
    pub closed_tcp_connections: usize,
    pub total_closed_tcp_connections: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDescriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

struct Virtqueue {
    index: u16,
    size: u16,
    virt: *mut u8,
    used_offset: usize,
    available_index: u16,
    used_index: u16,
}

impl Virtqueue {
    fn allocate(io_base: u16, index: u16) -> Result<Self, NetworkError> {
        unsafe {
            outw(io_base + VIRTIO_QUEUE_SELECT, index);
        }
        let size = unsafe { inw(io_base + VIRTIO_QUEUE_SIZE) };
        if size == 0 {
            return Err(NetworkError::QueueUnavailable(index));
        }
        if size < 1 {
            return Err(NetworkError::QueueTooSmall(index));
        }
        let (queue_bytes, used_offset) = virtqueue_layout(size);
        let pages = queue_bytes.div_ceil(bootinfo::PAGE_SIZE as usize) as u64;
        let physical =
            memory::allocate_contiguous_pages(pages).ok_or(NetworkError::DmaAddressUnavailable)?;
        let pfn = u32::try_from(physical >> 12).map_err(|_| NetworkError::QueueAddressTooHigh)?;
        let virt = vm::physical_to_high_half(physical)
            .map(|address| address as *mut u8)
            .ok_or(NetworkError::DmaAddressUnavailable)?;
        unsafe {
            core::ptr::write_bytes(virt, 0, pages as usize * bootinfo::PAGE_SIZE as usize);
            outl(io_base + VIRTIO_QUEUE_PFN, pfn);
        }
        Ok(Self {
            index,
            size,
            virt,
            used_offset,
            available_index: 0,
            used_index: 0,
        })
    }

    fn set_descriptor(&mut self, descriptor: VirtqDescriptor) {
        unsafe {
            core::ptr::write_volatile(self.virt.cast::<VirtqDescriptor>(), descriptor);
        }
    }

    fn submit(&mut self, io_base: u16) {
        let descriptor_bytes = core::mem::size_of::<VirtqDescriptor>() * usize::from(self.size);
        let available = unsafe { self.virt.add(descriptor_bytes) };
        let slot = usize::from(self.available_index % self.size);
        unsafe {
            core::ptr::write_volatile(available.add(4 + slot * 2).cast::<u16>(), 0);
        }
        fence(Ordering::Release);
        self.available_index = self.available_index.wrapping_add(1);
        unsafe {
            core::ptr::write_volatile(available.add(2).cast::<u16>(), self.available_index);
            outw(io_base + VIRTIO_QUEUE_NOTIFY, self.index);
        }
    }

    fn try_completion(&mut self) -> Result<Option<u32>, NetworkError> {
        let used = unsafe { self.virt.add(self.used_offset) };
        fence(Ordering::Acquire);
        let device_index = unsafe { core::ptr::read_volatile(used.add(2).cast::<u16>()) };
        if device_index == self.used_index {
            return Ok(None);
        }
        if device_index != self.used_index.wrapping_add(1) {
            return Err(NetworkError::DeviceRejected);
        }
        let slot = usize::from(self.used_index % self.size);
        let descriptor = unsafe { core::ptr::read_volatile(used.add(4 + slot * 8).cast::<u32>()) };
        let length = unsafe { core::ptr::read_volatile(used.add(8 + slot * 8).cast::<u32>()) };
        if descriptor != 0 {
            return Err(NetworkError::DeviceRejected);
        }
        self.used_index = device_index;
        Ok(Some(length))
    }

    fn wait_completion(&mut self, io_base: u16) -> Result<u32, NetworkError> {
        for spin in 0..DEVICE_TIMEOUT_SPINS {
            if let Some(length) = self.try_completion()? {
                return Ok(length);
            }
            if spin & 0x3fff == 0 {
                unsafe {
                    let _ = inb(io_base + VIRTIO_ISR_STATUS);
                }
            }
            core::hint::spin_loop();
        }
        Err(NetworkError::RequestTimedOut)
    }
}

struct VirtioNet {
    pci: hardware::PciAddress,
    io_base: u16,
    mac: [u8; 6],
    rx: Virtqueue,
    tx: Virtqueue,
    rx_buffer_phys: u64,
    rx_buffer_virt: *mut u8,
    tx_buffer_phys: u64,
    tx_buffer_virt: *mut u8,
    transmitted_frames: u64,
    received_frames: u64,
}

impl VirtioNet {
    fn discover() -> Result<Self, NetworkError> {
        let pci = find_legacy_virtio_net().ok_or(NetworkError::DeviceNotFound)?;
        let bar0 = hardware::pci_read_u32(pci, 0x10);
        if bar0 & 1 == 0 {
            return Err(NetworkError::InvalidIoBar);
        }
        let io_base = u16::try_from(bar0 & !3).map_err(|_| NetworkError::InvalidIoBar)?;
        hardware::enable_io_bus_master(pci);

        unsafe {
            outb(io_base + VIRTIO_DEVICE_STATUS, 0);
            outb(
                io_base + VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
            );
        }
        let host_features = unsafe { inl(io_base + VIRTIO_HOST_FEATURES) };
        if host_features & VIRTIO_NET_F_MAC == 0 {
            return Err(NetworkError::DeviceRejected);
        }
        unsafe {
            outl(io_base + VIRTIO_GUEST_FEATURES, VIRTIO_NET_F_MAC);
        }

        let rx = Virtqueue::allocate(io_base, RX_QUEUE_INDEX)?;
        let tx = Virtqueue::allocate(io_base, TX_QUEUE_INDEX)?;
        let (rx_buffer_phys, rx_buffer_virt) = allocate_dma_page()?;
        let (tx_buffer_phys, tx_buffer_virt) = allocate_dma_page()?;
        let mut mac = [0_u8; 6];
        for (offset, byte) in mac.iter_mut().enumerate() {
            *byte = unsafe { inb(io_base + VIRTIO_DEVICE_CONFIG + offset as u16) };
        }
        if mac == [0; 6] || mac == [0xff; 6] || mac[0] & 1 != 0 {
            return Err(NetworkError::DeviceRejected);
        }

        unsafe {
            outb(
                io_base + VIRTIO_DEVICE_STATUS,
                VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
            );
        }
        let status = unsafe { inb(io_base + VIRTIO_DEVICE_STATUS) };
        if status & VIRTIO_STATUS_FAILED != 0 || status & VIRTIO_STATUS_DRIVER_OK == 0 {
            return Err(NetworkError::DeviceRejected);
        }

        let mut device = Self {
            pci,
            io_base,
            mac,
            rx,
            tx,
            rx_buffer_phys,
            rx_buffer_virt,
            tx_buffer_phys,
            tx_buffer_virt,
            transmitted_frames: 0,
            received_frames: 0,
        };
        device.post_receive_buffer();
        Ok(device)
    }

    fn post_receive_buffer(&mut self) {
        self.rx.set_descriptor(VirtqDescriptor {
            address: self.rx_buffer_phys,
            length: DMA_BUFFER_BYTES as u32,
            flags: VIRTQ_DESC_F_WRITE,
            next: 0,
        });
        self.rx.submit(self.io_base);
    }

    fn transmit(&mut self, frame: &[u8]) -> Result<(), NetworkError> {
        if frame.len() > MAX_ETHERNET_FRAME
            || frame.len() + VIRTIO_NET_HEADER_BYTES > DMA_BUFFER_BYTES
        {
            return Err(NetworkError::FrameTooLarge);
        }
        unsafe {
            core::ptr::write_bytes(self.tx_buffer_virt, 0, VIRTIO_NET_HEADER_BYTES);
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                self.tx_buffer_virt.add(VIRTIO_NET_HEADER_BYTES),
                frame.len(),
            );
        }
        self.tx.set_descriptor(VirtqDescriptor {
            address: self.tx_buffer_phys,
            length: (VIRTIO_NET_HEADER_BYTES + frame.len()) as u32,
            flags: 0,
            next: 0,
        });
        self.tx.submit(self.io_base);
        let _ = self.tx.wait_completion(self.io_base)?;
        self.transmitted_frames = self.transmitted_frames.saturating_add(1);
        Ok(())
    }

    fn try_receive(&mut self, output: &mut [u8]) -> Result<Option<usize>, NetworkError> {
        let Some(length) = self.rx.try_completion()? else {
            return Ok(None);
        };
        let length = usize::try_from(length).map_err(|_| NetworkError::PacketMalformed)?;
        if !(VIRTIO_NET_HEADER_BYTES..=DMA_BUFFER_BYTES).contains(&length) {
            return Err(NetworkError::PacketMalformed);
        }
        let frame_length = length - VIRTIO_NET_HEADER_BYTES;
        if frame_length > output.len() || frame_length > MAX_ETHERNET_FRAME {
            return Err(NetworkError::FrameTooLarge);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.rx_buffer_virt.add(VIRTIO_NET_HEADER_BYTES),
                output.as_mut_ptr(),
                frame_length,
            );
        }
        self.received_frames = self.received_frames.saturating_add(1);
        self.post_receive_buffer();
        Ok(Some(frame_length))
    }
}

#[derive(Clone, Copy)]
struct DhcpLease {
    address: [u8; 4],
    subnet_mask: [u8; 4],
    gateway: [u8; 4],
    server: [u8; 4],
    dns_server: [u8; 4],
    lease_seconds: u32,
}

#[derive(Default)]
struct DhcpOptions {
    message_type: Option<u8>,
    subnet_mask: Option<[u8; 4]>,
    gateway: Option<[u8; 4]>,
    server: Option<[u8; 4]>,
    dns_server: Option<[u8; 4]>,
    lease_seconds: Option<u32>,
}

struct DhcpMessage {
    offered_address: [u8; 4],
    options: DhcpOptions,
}

#[derive(Clone, Copy)]
struct TcpSegment {
    sequence: u32,
    acknowledgement: u32,
    flags: u16,
    payload_offset: usize,
    payload_len: usize,
}

#[derive(Clone, Copy)]
struct TcpFrameRequest<'a> {
    source_mac: [u8; 6],
    destination_mac: [u8; 6],
    source_address: [u8; 4],
    destination_address: [u8; 4],
    source_port: u16,
    destination_port: u16,
    sequence: u32,
    acknowledgement: u32,
    flags: u16,
    payload: &'a [u8],
}

#[derive(Clone, Copy)]
struct IncomingTcpSegment {
    remote_mac: [u8; 6],
    remote_address: [u8; 4],
    remote_port: u16,
    segment: TcpSegment,
}

#[derive(Clone, Copy)]
struct TcpLocalEndpoint {
    mac: [u8; 6],
    address: [u8; 4],
}

#[derive(Clone, Copy)]
struct TcpServerConnection {
    remote_mac: [u8; 6],
    remote_address: [u8; 4],
    remote_port: u16,
    remote_next: u32,
    local_next: u32,
    phase: TcpServerPhase,
    last_activity_tick: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TcpServerPhase {
    Established,
    FinWait1,
    FinWait2,
}

struct TcpServerAction {
    frame: [u8; MAX_ETHERNET_FRAME],
    length: usize,
    event: Option<TcpServerEvent>,
}

struct TcpListener {
    local_port: u16,
    connections: [Option<TcpServerConnection>; TCP_LISTENER_CAPACITY],
    served_connections: u64,
    closed_connections: u64,
}

impl TcpListener {
    const fn new(local_port: u16) -> Self {
        Self {
            local_port,
            connections: [None; TCP_LISTENER_CAPACITY],
            served_connections: 0,
            closed_connections: 0,
        }
    }

    #[cfg(test)]
    fn handle_frame(
        &mut self,
        frame: &[u8],
        local_mac: [u8; 6],
        local_address: [u8; 4],
    ) -> Result<Option<TcpServerAction>, NetworkError> {
        self.handle_frame_at(frame, local_mac, local_address, 0)
    }

    fn handle_frame_at(
        &mut self,
        frame: &[u8],
        local_mac: [u8; 6],
        local_address: [u8; 4],
        now_tick: u64,
    ) -> Result<Option<TcpServerAction>, NetworkError> {
        let incoming =
            parse_tcp_listener_segment(frame, local_mac, local_address, self.local_port)?;
        let connection_index = self.connection_index(&incoming);
        if incoming.segment.flags & TCP_FLAG_RST != 0 {
            if let Some(index) = connection_index {
                self.connections[index] = None;
            }
            return Ok(None);
        }

        if incoming.segment.flags & TCP_FLAG_SYN != 0 {
            return self
                .accept_syn(incoming, local_mac, local_address, now_tick)
                .map(Some);
        }

        let Some(connection_index) = connection_index else {
            return self.reset(incoming, local_mac, local_address).map(Some);
        };
        let Some(connection) = self.connections[connection_index] else {
            return Ok(None);
        };
        if incoming.segment.payload_len == 0 {
            return self.handle_control_segment(
                connection_index,
                connection,
                incoming,
                local_mac,
                local_address,
                now_tick,
            );
        }
        let payload = &frame[incoming.segment.payload_offset
            ..incoming.segment.payload_offset + incoming.segment.payload_len];
        if connection.phase != TcpServerPhase::Established {
            return self.retransmit_response(
                connection_index,
                connection,
                incoming,
                payload,
                TcpLocalEndpoint {
                    mac: local_mac,
                    address: local_address,
                },
                now_tick,
            );
        }
        if incoming.segment.acknowledgement != connection.local_next
            || incoming.segment.sequence != connection.remote_next
        {
            return Ok(None);
        }
        if !is_supported_listener_request(payload) {
            self.connections[connection_index] = None;
            return self.reset(incoming, local_mac, local_address).map(Some);
        }
        let remote_next = connection
            .remote_next
            .wrapping_add(incoming.segment.payload_len as u32)
            .wrapping_add(u32::from(incoming.segment.flags & TCP_FLAG_FIN != 0));
        let (reply, length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: local_mac,
            destination_mac: connection.remote_mac,
            source_address: local_address,
            destination_address: connection.remote_address,
            source_port: self.local_port,
            destination_port: connection.remote_port,
            sequence: connection.local_next,
            acknowledgement: remote_next,
            flags: TCP_FLAG_PSH | TCP_FLAG_ACK | TCP_FLAG_FIN,
            payload: HTTP_LISTENER_RESPONSE,
        })?;
        self.connections[connection_index] = Some(TcpServerConnection {
            remote_next,
            local_next: connection
                .local_next
                .wrapping_add(HTTP_LISTENER_RESPONSE.len() as u32)
                .wrapping_add(1),
            phase: TcpServerPhase::FinWait1,
            last_activity_tick: now_tick,
            ..connection
        });
        self.served_connections = self.served_connections.saturating_add(1);
        Ok(Some(TcpServerAction {
            frame: reply,
            length,
            event: Some(TcpServerEvent {
                kind: TcpServerEventKind::Served,
                remote_address: connection.remote_address,
                remote_port: connection.remote_port,
                local_port: self.local_port,
                request_bytes: incoming.segment.payload_len,
                response_bytes: HTTP_LISTENER_RESPONSE.len(),
                connection_count: self.served_connections,
            }),
        }))
    }

    fn accept_syn(
        &mut self,
        incoming: IncomingTcpSegment,
        local_mac: [u8; 6],
        local_address: [u8; 4],
        now_tick: u64,
    ) -> Result<TcpServerAction, NetworkError> {
        let Some(connection_index) = self.connection_index(&incoming).or_else(|| {
            self.connections
                .iter()
                .position(core::option::Option::is_none)
        }) else {
            return self.reset(incoming, local_mac, local_address);
        };
        let server_sequence = tcp_server_sequence(incoming.remote_address, incoming.remote_port);
        let remote_next = incoming.segment.sequence.wrapping_add(1);
        let (reply, length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: local_mac,
            destination_mac: incoming.remote_mac,
            source_address: local_address,
            destination_address: incoming.remote_address,
            source_port: self.local_port,
            destination_port: incoming.remote_port,
            sequence: server_sequence,
            acknowledgement: remote_next,
            flags: TCP_FLAG_SYN | TCP_FLAG_ACK,
            payload: &[],
        })?;
        self.connections[connection_index] = Some(TcpServerConnection {
            remote_mac: incoming.remote_mac,
            remote_address: incoming.remote_address,
            remote_port: incoming.remote_port,
            remote_next,
            local_next: server_sequence.wrapping_add(1),
            phase: TcpServerPhase::Established,
            last_activity_tick: now_tick,
        });
        Ok(TcpServerAction {
            frame: reply,
            length,
            event: Some(TcpServerEvent {
                kind: TcpServerEventKind::Accepted,
                remote_address: incoming.remote_address,
                remote_port: incoming.remote_port,
                local_port: self.local_port,
                request_bytes: 0,
                response_bytes: 0,
                connection_count: self.served_connections,
            }),
        })
    }

    fn reset(
        &self,
        incoming: IncomingTcpSegment,
        local_mac: [u8; 6],
        local_address: [u8; 4],
    ) -> Result<TcpServerAction, NetworkError> {
        let acknowledgement = incoming
            .segment
            .sequence
            .wrapping_add(incoming.segment.payload_len as u32)
            .wrapping_add(u32::from(incoming.segment.flags & TCP_FLAG_SYN != 0))
            .wrapping_add(u32::from(incoming.segment.flags & TCP_FLAG_FIN != 0));
        let (reply, length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: local_mac,
            destination_mac: incoming.remote_mac,
            source_address: local_address,
            destination_address: incoming.remote_address,
            source_port: self.local_port,
            destination_port: incoming.remote_port,
            sequence: tcp_server_sequence(incoming.remote_address, incoming.remote_port),
            acknowledgement,
            flags: TCP_FLAG_RST | TCP_FLAG_ACK,
            payload: &[],
        })?;
        Ok(TcpServerAction {
            frame: reply,
            length,
            event: None,
        })
    }

    fn connection_index(&self, incoming: &IncomingTcpSegment) -> Option<usize> {
        self.connections.iter().position(|entry| {
            entry.is_some_and(|connection| {
                connection.remote_address == incoming.remote_address
                    && connection.remote_port == incoming.remote_port
                    && connection.remote_mac == incoming.remote_mac
            })
        })
    }

    fn handle_control_segment(
        &mut self,
        connection_index: usize,
        mut connection: TcpServerConnection,
        incoming: IncomingTcpSegment,
        local_mac: [u8; 6],
        local_address: [u8; 4],
        now_tick: u64,
    ) -> Result<Option<TcpServerAction>, NetworkError> {
        if incoming.segment.flags & TCP_FLAG_ACK == 0
            || incoming.segment.acknowledgement != connection.local_next
            || incoming.segment.sequence != connection.remote_next
        {
            return Ok(None);
        }
        connection.last_activity_tick = now_tick;
        if incoming.segment.flags & TCP_FLAG_FIN != 0 {
            connection.remote_next = connection.remote_next.wrapping_add(1);
            self.connections[connection_index] = None;
            self.closed_connections = self.closed_connections.saturating_add(1);
            return build_tcp_ipv4_frame(TcpFrameRequest {
                source_mac: local_mac,
                destination_mac: connection.remote_mac,
                source_address: local_address,
                destination_address: connection.remote_address,
                source_port: self.local_port,
                destination_port: connection.remote_port,
                sequence: connection.local_next,
                acknowledgement: connection.remote_next,
                flags: TCP_FLAG_ACK,
                payload: &[],
            })
            .map(|(frame, length)| {
                Some(TcpServerAction {
                    frame,
                    length,
                    event: None,
                })
            });
        }
        if connection.phase == TcpServerPhase::FinWait1 {
            connection.phase = TcpServerPhase::FinWait2;
        }
        self.connections[connection_index] = Some(connection);
        Ok(None)
    }

    fn retransmit_response(
        &mut self,
        connection_index: usize,
        mut connection: TcpServerConnection,
        incoming: IncomingTcpSegment,
        payload: &[u8],
        local: TcpLocalEndpoint,
        now_tick: u64,
    ) -> Result<Option<TcpServerAction>, NetworkError> {
        let remote_span = (incoming.segment.payload_len as u32)
            .wrapping_add(u32::from(incoming.segment.flags & TCP_FLAG_FIN != 0));
        let request_sequence = connection.remote_next.wrapping_sub(remote_span);
        let response_sequence = connection
            .local_next
            .wrapping_sub(HTTP_LISTENER_RESPONSE.len() as u32)
            .wrapping_sub(1);
        if !is_supported_listener_request(payload)
            || incoming.segment.sequence != request_sequence
            || incoming.segment.acknowledgement != response_sequence
        {
            return Ok(None);
        }
        let (frame, length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: local.mac,
            destination_mac: connection.remote_mac,
            source_address: local.address,
            destination_address: connection.remote_address,
            source_port: self.local_port,
            destination_port: connection.remote_port,
            sequence: response_sequence,
            acknowledgement: connection.remote_next,
            flags: TCP_FLAG_PSH | TCP_FLAG_ACK | TCP_FLAG_FIN,
            payload: HTTP_LISTENER_RESPONSE,
        })?;
        connection.last_activity_tick = now_tick;
        self.connections[connection_index] = Some(connection);
        Ok(Some(TcpServerAction {
            frame,
            length,
            event: None,
        }))
    }

    fn expire_idle_connections(&mut self, now_tick: u64) -> usize {
        let mut expired = 0;
        for connection in &mut self.connections {
            if connection.is_some_and(|entry| {
                now_tick.saturating_sub(entry.last_activity_tick) >= TCP_LISTENER_IDLE_TIMEOUT_TICKS
            }) {
                *connection = None;
                expired += 1;
            }
        }
        expired
    }
}

#[derive(Clone, Copy)]
struct HttpProof {
    status: u16,
    response_bytes: usize,
}

#[derive(Clone, Copy)]
struct HttpResponse {
    segment: TcpSegment,
    proof: HttpProof,
}

pub struct NetworkInterface {
    device: VirtioNet,
    lease: DhcpLease,
    gateway_mac: [u8; 6],
    dns_answer: [u8; 4],
    http_status: u16,
    http_response_bytes: usize,
    tcp_listener: TcpListener,
}

impl NetworkInterface {
    pub fn report(&self) -> NetworkReport {
        NetworkReport {
            pci_bus: self.device.pci.bus,
            pci_device: self.device.pci.device,
            pci_function: self.device.pci.function,
            io_base: self.device.io_base,
            mac: self.device.mac,
            address: self.lease.address,
            subnet_mask: self.lease.subnet_mask,
            gateway: self.lease.gateway,
            dhcp_server: self.lease.server,
            dns_server: self.lease.dns_server,
            dns_answer: self.dns_answer,
            dns_query_id: DNS_QUERY_ID,
            http_status: self.http_status,
            http_response_bytes: self.http_response_bytes,
            tcp_source_port: HTTP_CLIENT_PORT,
            gateway_mac: self.gateway_mac,
            icmp_echo_sequence: ICMP_ECHO_SEQUENCE,
            lease_seconds: self.lease.lease_seconds,
            transmitted_frames: self.device.transmitted_frames,
            received_frames: self.device.received_frames,
        }
    }

    pub fn poll(&mut self) -> Result<NetworkPollResult, NetworkError> {
        let mut frame = [0_u8; MAX_ETHERNET_FRAME];
        let now_tick = crate::interrupts::status().ticks;
        let closed_before = self.tcp_listener.closed_connections;
        let mut result = NetworkPollResult {
            received_frame: false,
            tcp_server: None,
            expired_tcp_connections: self.tcp_listener.expire_idle_connections(now_tick),
            closed_tcp_connections: 0,
            total_closed_tcp_connections: closed_before,
        };
        for _ in 0..32 {
            let Some(length) = self.device.try_receive(&mut frame)? else {
                break;
            };
            result.received_frame = true;
            let frame = &frame[..length];
            if let Some(reply) = build_arp_reply(frame, self.device.mac, self.lease.address) {
                self.device.transmit(&reply)?;
            } else if let Some((reply, reply_length)) =
                build_icmp_echo_reply(frame, self.device.mac, self.lease.address)
            {
                self.device.transmit(&reply[..reply_length])?;
            } else {
                match self.tcp_listener.handle_frame_at(
                    frame,
                    self.device.mac,
                    self.lease.address,
                    now_tick,
                ) {
                    Ok(Some(action)) => {
                        self.device.transmit(&action.frame[..action.length])?;
                        if let Some(event) = action.event
                            && (result.tcp_server.is_none()
                                || event.kind == TcpServerEventKind::Served)
                        {
                            result.tcp_server = Some(event);
                        }
                    }
                    Ok(None) | Err(NetworkError::PacketMalformed) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        result.total_closed_tcp_connections = self.tcp_listener.closed_connections;
        result.closed_tcp_connections = usize::try_from(
            result
                .total_closed_tcp_connections
                .saturating_sub(closed_before),
        )
        .unwrap_or(usize::MAX);
        Ok(result)
    }
}

pub fn configure() -> Result<NetworkInterface, NetworkError> {
    let mut device = VirtioNet::discover()?;
    let offer_frame = build_dhcp_frame(device.mac, DHCP_DISCOVER, None, None);
    device.transmit(&offer_frame)?;
    let offer = wait_for_dhcp(&mut device, DHCP_OFFER)?;
    let server = offer.options.server.ok_or(NetworkError::DhcpRejected)?;
    if offer.offered_address == [0; 4] {
        return Err(NetworkError::DhcpRejected);
    }

    let request_frame = build_dhcp_frame(
        device.mac,
        DHCP_REQUEST,
        Some(offer.offered_address),
        Some(server),
    );
    device.transmit(&request_frame)?;
    let acknowledgement = wait_for_dhcp(&mut device, DHCP_ACK)?;
    if acknowledgement.offered_address != offer.offered_address {
        return Err(NetworkError::DhcpRejected);
    }
    let lease = DhcpLease {
        address: acknowledgement.offered_address,
        subnet_mask: acknowledgement
            .options
            .subnet_mask
            .or(offer.options.subnet_mask)
            .ok_or(NetworkError::DhcpRejected)?,
        gateway: acknowledgement
            .options
            .gateway
            .or(offer.options.gateway)
            .ok_or(NetworkError::DhcpRejected)?,
        server: acknowledgement.options.server.unwrap_or(server),
        dns_server: acknowledgement
            .options
            .dns_server
            .or(offer.options.dns_server)
            .unwrap_or_else(|| {
                default_dns_server(
                    offer.offered_address,
                    acknowledgement
                        .options
                        .gateway
                        .or(offer.options.gateway)
                        .unwrap_or([10, 0, 2, 2]),
                )
            }),
        lease_seconds: acknowledgement
            .options
            .lease_seconds
            .or(offer.options.lease_seconds)
            .ok_or(NetworkError::DhcpRejected)?,
    };

    let arp = build_arp_request(device.mac, lease.address, lease.gateway);
    device.transmit(&arp)?;
    let gateway_mac = wait_for_arp_reply(&mut device, lease.address, lease.gateway)?;
    let echo = build_icmp_echo_request(device.mac, gateway_mac, lease.address, lease.gateway);
    device.transmit(&echo)?;
    wait_for_icmp_echo_reply(&mut device, gateway_mac, lease.address, lease.gateway)?;
    let dns_next_hop = dns_next_hop(
        lease.address,
        lease.subnet_mask,
        lease.gateway,
        lease.dns_server,
    );
    let dns_mac = if dns_next_hop == lease.gateway {
        gateway_mac
    } else {
        let arp = build_arp_request(device.mac, lease.address, dns_next_hop);
        device.transmit(&arp)?;
        wait_for_arp_reply(&mut device, lease.address, dns_next_hop)?
    };
    let (dns_query, dns_query_length) = build_dns_query_frame(
        device.mac,
        dns_mac,
        lease.address,
        lease.dns_server,
        DNS_QUERY_ID,
        DNS_QUERY_NAME,
    )?;
    device.transmit(&dns_query[..dns_query_length])?;
    let dns_answer = wait_for_dns_response(
        &mut device,
        dns_mac,
        lease.address,
        lease.dns_server,
        DNS_QUERY_ID,
        DNS_QUERY_NAME,
    )?;
    let http_next_hop = route_next_hop(lease.address, lease.subnet_mask, lease.gateway, dns_answer);
    let http_mac = if http_next_hop == lease.gateway {
        gateway_mac
    } else {
        let arp = build_arp_request(device.mac, lease.address, http_next_hop);
        device.transmit(&arp)?;
        wait_for_arp_reply(&mut device, lease.address, http_next_hop)?
    };
    let http = perform_http_get(
        &mut device,
        http_mac,
        lease.address,
        dns_answer,
        DNS_QUERY_NAME,
    )?;
    Ok(NetworkInterface {
        device,
        lease,
        gateway_mac,
        dns_answer,
        http_status: http.status,
        http_response_bytes: http.response_bytes,
        tcp_listener: TcpListener::new(TCP_LISTENER_PORT),
    })
}

fn build_dhcp_frame(
    mac: [u8; 6],
    message_type: u8,
    requested_address: Option<[u8; 4]>,
    server: Option<[u8; 4]>,
) -> [u8; ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + UDP_HEADER_BYTES + DHCP_PAYLOAD_BYTES] {
    const FRAME_BYTES: usize =
        ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + UDP_HEADER_BYTES + DHCP_PAYLOAD_BYTES;
    let mut frame = [0_u8; FRAME_BYTES];
    frame[0..6].fill(0xff);
    frame[6..12].copy_from_slice(&mac);
    put_be_u16(&mut frame, 12, ETHERTYPE_IPV4);

    let ip = ETHERNET_HEADER_BYTES;
    frame[ip] = 0x45;
    put_be_u16(
        &mut frame,
        ip + 2,
        (IPV4_HEADER_BYTES + UDP_HEADER_BYTES + DHCP_PAYLOAD_BYTES) as u16,
    );
    put_be_u16(&mut frame, ip + 4, (DHCP_XID & 0xffff) as u16);
    put_be_u16(&mut frame, ip + 6, 0x4000);
    frame[ip + 8] = 64;
    frame[ip + 9] = IP_PROTOCOL_UDP;
    frame[ip + 16..ip + 20].fill(0xff);
    let checksum = internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]);
    put_be_u16(&mut frame, ip + 10, checksum);

    let udp = ip + IPV4_HEADER_BYTES;
    put_be_u16(&mut frame, udp, DHCP_CLIENT_PORT);
    put_be_u16(&mut frame, udp + 2, DHCP_SERVER_PORT);
    put_be_u16(
        &mut frame,
        udp + 4,
        (UDP_HEADER_BYTES + DHCP_PAYLOAD_BYTES) as u16,
    );

    let dhcp = udp + UDP_HEADER_BYTES;
    frame[dhcp] = 1;
    frame[dhcp + 1] = 1;
    frame[dhcp + 2] = 6;
    put_be_u32(&mut frame, dhcp + 4, DHCP_XID);
    put_be_u16(&mut frame, dhcp + 10, 0x8000);
    frame[dhcp + 28..dhcp + 34].copy_from_slice(&mac);
    frame[dhcp + 236..dhcp + 240].copy_from_slice(&[99, 130, 83, 99]);
    let mut option = dhcp + 240;
    write_option(&mut frame, &mut option, 53, &[message_type]);
    let mut client_identifier = [0_u8; 7];
    client_identifier[0] = 1;
    client_identifier[1..].copy_from_slice(&mac);
    write_option(&mut frame, &mut option, 61, &client_identifier);
    write_option(&mut frame, &mut option, 12, b"codexos");
    write_option(&mut frame, &mut option, 55, &[1, 3, 6, 51, 54]);
    if let Some(address) = requested_address {
        write_option(&mut frame, &mut option, 50, &address);
    }
    if let Some(server) = server {
        write_option(&mut frame, &mut option, 54, &server);
    }
    frame[option] = 255;
    frame
}

fn wait_for_dhcp(device: &mut VirtioNet, expected_type: u8) -> Result<DhcpMessage, NetworkError> {
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    for _ in 0..PROTOCOL_TIMEOUT_SPINS {
        if let Some(length) = device.try_receive(&mut frame)?
            && let Ok(message) = parse_dhcp_message(&frame[..length], device.mac)
            && message.options.message_type == Some(expected_type)
        {
            return Ok(message);
        }
        core::hint::spin_loop();
    }
    Err(NetworkError::DhcpTimedOut)
}

fn parse_dhcp_message(frame: &[u8], mac: [u8; 6]) -> Result<DhcpMessage, NetworkError> {
    if frame.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + UDP_HEADER_BYTES + 240
        || get_be_u16(frame, 12)? != ETHERTYPE_IPV4
    {
        return Err(NetworkError::PacketMalformed);
    }
    let ip = ETHERNET_HEADER_BYTES;
    let header_bytes = usize::from(frame[ip] & 0x0f) * 4;
    if frame[ip] >> 4 != 4
        || header_bytes < IPV4_HEADER_BYTES
        || ip + header_bytes > frame.len()
        || internet_checksum(&frame[ip..ip + header_bytes]) != 0
        || frame[ip + 9] != IP_PROTOCOL_UDP
        || get_be_u16(frame, ip + 6)? & 0x3fff != 0
    {
        return Err(NetworkError::PacketMalformed);
    }
    let ip_length = usize::from(get_be_u16(frame, ip + 2)?);
    if ip_length < header_bytes + UDP_HEADER_BYTES || ip + ip_length > frame.len() {
        return Err(NetworkError::PacketMalformed);
    }
    let udp = ip + header_bytes;
    let udp_length = usize::from(get_be_u16(frame, udp + 4)?);
    if get_be_u16(frame, udp)? != DHCP_SERVER_PORT
        || get_be_u16(frame, udp + 2)? != DHCP_CLIENT_PORT
        || udp_length < UDP_HEADER_BYTES + 240
        || udp + udp_length > ip + ip_length
    {
        return Err(NetworkError::PacketMalformed);
    }
    let udp_checksum = get_be_u16(frame, udp + 6)?;
    if udp_checksum != 0
        && ipv4_transport_checksum(
            read_ipv4(frame, ip + 12)?,
            read_ipv4(frame, ip + 16)?,
            IP_PROTOCOL_UDP,
            &frame[udp..udp + udp_length],
        ) != 0
    {
        return Err(NetworkError::PacketMalformed);
    }
    let dhcp = udp + UDP_HEADER_BYTES;
    if frame[dhcp] != 2
        || frame[dhcp + 1] != 1
        || frame[dhcp + 2] != 6
        || get_be_u32(frame, dhcp + 4)? != DHCP_XID
        || frame.get(dhcp + 28..dhcp + 34) != Some(mac.as_slice())
        || frame.get(dhcp + 236..dhcp + 240) != Some(&[99, 130, 83, 99])
    {
        return Err(NetworkError::DhcpRejected);
    }
    let offered_address = read_ipv4(frame, dhcp + 16)?;
    let options = parse_dhcp_options(&frame[dhcp + 240..udp + udp_length])?;
    Ok(DhcpMessage {
        offered_address,
        options,
    })
}

fn parse_dhcp_options(bytes: &[u8]) -> Result<DhcpOptions, NetworkError> {
    let mut options = DhcpOptions::default();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let code = bytes[cursor];
        cursor += 1;
        if code == 0 {
            continue;
        }
        if code == 255 {
            return Ok(options);
        }
        let length = usize::from(*bytes.get(cursor).ok_or(NetworkError::PacketMalformed)?);
        cursor += 1;
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or(NetworkError::PacketMalformed)?;
        let value = &bytes[cursor..end];
        match (code, value) {
            (53, [message_type]) => options.message_type = Some(*message_type),
            (1, [a, b, c, d]) => options.subnet_mask = Some([*a, *b, *c, *d]),
            (3, [a, b, c, d, ..]) => options.gateway = Some([*a, *b, *c, *d]),
            (6, [a, b, c, d, ..]) => options.dns_server = Some([*a, *b, *c, *d]),
            (54, [a, b, c, d]) => options.server = Some([*a, *b, *c, *d]),
            (51, [a, b, c, d]) => {
                options.lease_seconds = Some(u32::from_be_bytes([*a, *b, *c, *d]));
            }
            _ => {}
        }
        cursor = end;
    }
    Err(NetworkError::PacketMalformed)
}

fn build_arp_request(mac: [u8; 6], address: [u8; 4], target: [u8; 4]) -> [u8; MIN_ETHERNET_FRAME] {
    let mut frame = [0_u8; MIN_ETHERNET_FRAME];
    frame[0..6].fill(0xff);
    frame[6..12].copy_from_slice(&mac);
    put_be_u16(&mut frame, 12, ETHERTYPE_ARP);
    put_be_u16(&mut frame, 14, 1);
    put_be_u16(&mut frame, 16, ETHERTYPE_IPV4);
    frame[18] = 6;
    frame[19] = 4;
    put_be_u16(&mut frame, 20, 1);
    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&address);
    frame[38..42].copy_from_slice(&target);
    frame
}

fn wait_for_arp_reply(
    device: &mut VirtioNet,
    address: [u8; 4],
    target: [u8; 4],
) -> Result<[u8; 6], NetworkError> {
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    for _ in 0..PROTOCOL_TIMEOUT_SPINS {
        if let Some(length) = device.try_receive(&mut frame)?
            && let Ok(mac) = parse_arp_reply(&frame[..length], address, target)
        {
            return Ok(mac);
        }
        core::hint::spin_loop();
    }
    Err(NetworkError::ArpTimedOut)
}

fn parse_arp_reply(
    frame: &[u8],
    address: [u8; 4],
    expected_sender: [u8; 4],
) -> Result<[u8; 6], NetworkError> {
    if frame.len() < 42
        || get_be_u16(frame, 12)? != ETHERTYPE_ARP
        || get_be_u16(frame, 14)? != 1
        || get_be_u16(frame, 16)? != ETHERTYPE_IPV4
        || frame[18] != 6
        || frame[19] != 4
        || get_be_u16(frame, 20)? != 2
        || frame.get(28..32) != Some(expected_sender.as_slice())
        || frame.get(38..42) != Some(address.as_slice())
    {
        return Err(NetworkError::PacketMalformed);
    }
    let mut mac = [0_u8; 6];
    mac.copy_from_slice(&frame[22..28]);
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(NetworkError::PacketMalformed);
    }
    Ok(mac)
}

fn build_arp_reply(
    frame: &[u8],
    local_mac: [u8; 6],
    local_address: [u8; 4],
) -> Option<[u8; MIN_ETHERNET_FRAME]> {
    if frame.len() < 42
        || get_be_u16(frame, 12).ok()? != ETHERTYPE_ARP
        || get_be_u16(frame, 14).ok()? != 1
        || get_be_u16(frame, 16).ok()? != ETHERTYPE_IPV4
        || frame[18] != 6
        || frame[19] != 4
        || get_be_u16(frame, 20).ok()? != 1
        || frame.get(6..12)? != frame.get(22..28)?
        || frame.get(38..42)? != local_address
    {
        return None;
    }
    let mut sender_mac = [0_u8; 6];
    sender_mac.copy_from_slice(frame.get(22..28)?);
    if sender_mac == [0; 6] || sender_mac[0] & 1 != 0 {
        return None;
    }
    let mut sender_address = [0_u8; 4];
    sender_address.copy_from_slice(frame.get(28..32)?);
    let mut reply = [0_u8; MIN_ETHERNET_FRAME];
    reply[0..6].copy_from_slice(&sender_mac);
    reply[6..12].copy_from_slice(&local_mac);
    put_be_u16(&mut reply, 12, ETHERTYPE_ARP);
    put_be_u16(&mut reply, 14, 1);
    put_be_u16(&mut reply, 16, ETHERTYPE_IPV4);
    reply[18] = 6;
    reply[19] = 4;
    put_be_u16(&mut reply, 20, 2);
    reply[22..28].copy_from_slice(&local_mac);
    reply[28..32].copy_from_slice(&local_address);
    reply[32..38].copy_from_slice(&sender_mac);
    reply[38..42].copy_from_slice(&sender_address);
    Some(reply)
}

fn build_icmp_echo_request(
    mac: [u8; 6],
    destination_mac: [u8; 6],
    address: [u8; 4],
    destination: [u8; 4],
) -> [u8; MIN_ETHERNET_FRAME] {
    let mut frame = [0_u8; MIN_ETHERNET_FRAME];
    frame[0..6].copy_from_slice(&destination_mac);
    frame[6..12].copy_from_slice(&mac);
    put_be_u16(&mut frame, 12, ETHERTYPE_IPV4);
    let ip = ETHERNET_HEADER_BYTES;
    frame[ip] = 0x45;
    put_be_u16(
        &mut frame,
        ip + 2,
        (IPV4_HEADER_BYTES + 8 + ICMP_ECHO_PAYLOAD.len()) as u16,
    );
    put_be_u16(&mut frame, ip + 4, ICMP_ECHO_IDENTIFIER);
    put_be_u16(&mut frame, ip + 6, 0x4000);
    frame[ip + 8] = 64;
    frame[ip + 9] = IP_PROTOCOL_ICMP;
    frame[ip + 12..ip + 16].copy_from_slice(&address);
    frame[ip + 16..ip + 20].copy_from_slice(&destination);
    let ip_checksum = internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]);
    put_be_u16(&mut frame, ip + 10, ip_checksum);

    let icmp = ip + IPV4_HEADER_BYTES;
    frame[icmp] = 8;
    put_be_u16(&mut frame, icmp + 4, ICMP_ECHO_IDENTIFIER);
    put_be_u16(&mut frame, icmp + 6, ICMP_ECHO_SEQUENCE);
    frame[icmp + 8..icmp + 8 + ICMP_ECHO_PAYLOAD.len()].copy_from_slice(ICMP_ECHO_PAYLOAD);
    let icmp_checksum = internet_checksum(&frame[icmp..icmp + 8 + ICMP_ECHO_PAYLOAD.len()]);
    put_be_u16(&mut frame, icmp + 2, icmp_checksum);
    frame
}

fn wait_for_icmp_echo_reply(
    device: &mut VirtioNet,
    expected_mac: [u8; 6],
    address: [u8; 4],
    expected_sender: [u8; 4],
) -> Result<(), NetworkError> {
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    for _ in 0..PROTOCOL_TIMEOUT_SPINS {
        if let Some(length) = device.try_receive(&mut frame)?
            && parse_icmp_echo_reply(&frame[..length], expected_mac, address, expected_sender)
                .is_ok()
        {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(NetworkError::IcmpTimedOut)
}

fn parse_icmp_echo_reply(
    frame: &[u8],
    expected_mac: [u8; 6],
    address: [u8; 4],
    expected_sender: [u8; 4],
) -> Result<(), NetworkError> {
    if frame.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + 8
        || frame.get(6..12) != Some(expected_mac.as_slice())
        || get_be_u16(frame, 12)? != ETHERTYPE_IPV4
    {
        return Err(NetworkError::PacketMalformed);
    }
    let ip = ETHERNET_HEADER_BYTES;
    let header_bytes = usize::from(frame[ip] & 0x0f) * 4;
    let total_bytes = usize::from(get_be_u16(frame, ip + 2)?);
    if frame[ip] >> 4 != 4
        || header_bytes < IPV4_HEADER_BYTES
        || total_bytes < header_bytes + 8
        || ip + total_bytes > frame.len()
        || internet_checksum(&frame[ip..ip + header_bytes]) != 0
        || frame[ip + 9] != IP_PROTOCOL_ICMP
        || get_be_u16(frame, ip + 6)? & 0x3fff != 0
        || frame.get(ip + 12..ip + 16) != Some(expected_sender.as_slice())
        || frame.get(ip + 16..ip + 20) != Some(address.as_slice())
    {
        return Err(NetworkError::PacketMalformed);
    }
    let icmp = ip + header_bytes;
    if internet_checksum(&frame[icmp..ip + total_bytes]) != 0
        || frame[icmp] != 0
        || frame[icmp + 1] != 0
        || get_be_u16(frame, icmp + 4)? != ICMP_ECHO_IDENTIFIER
        || get_be_u16(frame, icmp + 6)? != ICMP_ECHO_SEQUENCE
        || frame.get(icmp + 8..ip + total_bytes) != Some(ICMP_ECHO_PAYLOAD.as_slice())
    {
        return Err(NetworkError::PacketMalformed);
    }
    Ok(())
}

fn build_icmp_echo_reply(
    frame: &[u8],
    local_mac: [u8; 6],
    local_address: [u8; 4],
) -> Option<([u8; MAX_ETHERNET_FRAME], usize)> {
    if frame.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + 8
        || frame.len() > MAX_ETHERNET_FRAME
        || frame.get(0..6)? != local_mac
        || get_be_u16(frame, 12).ok()? != ETHERTYPE_IPV4
    {
        return None;
    }
    let ip = ETHERNET_HEADER_BYTES;
    let header_bytes = usize::from(frame[ip] & 0x0f) * 4;
    let total_bytes = usize::from(get_be_u16(frame, ip + 2).ok()?);
    if frame[ip] >> 4 != 4
        || header_bytes < IPV4_HEADER_BYTES
        || total_bytes < header_bytes + 8
        || ip + total_bytes > frame.len()
        || internet_checksum(frame.get(ip..ip + header_bytes)?) != 0
        || frame[ip + 9] != IP_PROTOCOL_ICMP
        || get_be_u16(frame, ip + 6).ok()? & 0x3fff != 0
        || frame.get(ip + 16..ip + 20)? != local_address
    {
        return None;
    }
    let icmp = ip + header_bytes;
    if internet_checksum(frame.get(icmp..ip + total_bytes)?) != 0
        || frame[icmp] != 8
        || frame[icmp + 1] != 0
    {
        return None;
    }

    let reply_length = frame.len().max(MIN_ETHERNET_FRAME);
    let mut reply = [0_u8; MAX_ETHERNET_FRAME];
    reply[..frame.len()].copy_from_slice(frame);
    reply[0..6].copy_from_slice(&frame[6..12]);
    reply[6..12].copy_from_slice(&local_mac);
    reply[ip + 12..ip + 16].copy_from_slice(&frame[ip + 16..ip + 20]);
    reply[ip + 16..ip + 20].copy_from_slice(&frame[ip + 12..ip + 16]);
    reply[ip + 8] = 64;
    put_be_u16(&mut reply, ip + 10, 0);
    let ip_checksum = internet_checksum(&reply[ip..ip + header_bytes]);
    put_be_u16(&mut reply, ip + 10, ip_checksum);
    reply[icmp] = 0;
    put_be_u16(&mut reply, icmp + 2, 0);
    let icmp_checksum = internet_checksum(&reply[icmp..ip + total_bytes]);
    put_be_u16(&mut reply, icmp + 2, icmp_checksum);
    Some((reply, reply_length))
}

fn build_dns_query_frame(
    mac: [u8; 6],
    destination_mac: [u8; 6],
    address: [u8; 4],
    dns_server: [u8; 4],
    query_id: u16,
    name: &str,
) -> Result<([u8; MAX_ETHERNET_FRAME], usize), NetworkError> {
    let mut dns = [0_u8; 256];
    put_be_u16(&mut dns, 0, query_id);
    put_be_u16(&mut dns, 2, 0x0100);
    put_be_u16(&mut dns, 4, 1);
    let mut dns_length = 12;
    write_dns_name(&mut dns, &mut dns_length, name)?;
    put_be_u16(&mut dns, dns_length, 1);
    put_be_u16(&mut dns, dns_length + 2, 1);
    dns_length += 4;
    build_udp_ipv4_frame(
        mac,
        destination_mac,
        address,
        dns_server,
        DNS_CLIENT_PORT,
        DNS_SERVER_PORT,
        &dns[..dns_length],
    )
}

fn perform_http_get(
    device: &mut VirtioNet,
    remote_mac: [u8; 6],
    address: [u8; 4],
    remote: [u8; 4],
    host: &str,
) -> Result<HttpProof, NetworkError> {
    let (syn, syn_length) = build_tcp_ipv4_frame(TcpFrameRequest {
        source_mac: device.mac,
        destination_mac: remote_mac,
        source_address: address,
        destination_address: remote,
        source_port: HTTP_CLIENT_PORT,
        destination_port: HTTP_SERVER_PORT,
        sequence: TCP_INITIAL_SEQUENCE,
        acknowledgement: 0,
        flags: TCP_FLAG_SYN,
        payload: &[],
    })?;
    device.transmit(&syn[..syn_length])?;
    let syn_ack = wait_for_tcp_segment(
        device,
        remote_mac,
        address,
        remote,
        HTTP_CLIENT_PORT,
        HTTP_SERVER_PORT,
        |segment, _| {
            segment.flags & (TCP_FLAG_SYN | TCP_FLAG_ACK) == (TCP_FLAG_SYN | TCP_FLAG_ACK)
                && segment.acknowledgement == TCP_INITIAL_SEQUENCE.wrapping_add(1)
        },
    )?;
    let remote_next = syn_ack.sequence.wrapping_add(1);
    let local_after_syn = TCP_INITIAL_SEQUENCE.wrapping_add(1);
    let (ack, ack_length) = build_tcp_ipv4_frame(TcpFrameRequest {
        source_mac: device.mac,
        destination_mac: remote_mac,
        source_address: address,
        destination_address: remote,
        source_port: HTTP_CLIENT_PORT,
        destination_port: HTTP_SERVER_PORT,
        sequence: local_after_syn,
        acknowledgement: remote_next,
        flags: TCP_FLAG_ACK,
        payload: &[],
    })?;
    device.transmit(&ack[..ack_length])?;

    let request = http_request_for_host(host)?;
    let (get, get_length) = build_tcp_ipv4_frame(TcpFrameRequest {
        source_mac: device.mac,
        destination_mac: remote_mac,
        source_address: address,
        destination_address: remote,
        source_port: HTTP_CLIENT_PORT,
        destination_port: HTTP_SERVER_PORT,
        sequence: local_after_syn,
        acknowledgement: remote_next,
        flags: TCP_FLAG_PSH | TCP_FLAG_ACK,
        payload: request,
    })?;
    device.transmit(&get[..get_length])?;
    let local_after_request = local_after_syn.wrapping_add(request.len() as u32);
    let response = wait_for_http_response(
        device,
        remote_mac,
        address,
        remote,
        HTTP_CLIENT_PORT,
        HTTP_SERVER_PORT,
        local_after_request,
    )?;
    let payload_ack = response
        .segment
        .sequence
        .wrapping_add(response.proof.response_bytes as u32)
        .wrapping_add(u32::from(response.segment.flags & TCP_FLAG_FIN != 0));
    let (final_ack, final_ack_length) = build_tcp_ipv4_frame(TcpFrameRequest {
        source_mac: device.mac,
        destination_mac: remote_mac,
        source_address: address,
        destination_address: remote,
        source_port: HTTP_CLIENT_PORT,
        destination_port: HTTP_SERVER_PORT,
        sequence: local_after_request,
        acknowledgement: payload_ack,
        flags: TCP_FLAG_ACK,
        payload: &[],
    })?;
    device.transmit(&final_ack[..final_ack_length])?;
    Ok(response.proof)
}

fn build_tcp_ipv4_frame(
    request: TcpFrameRequest<'_>,
) -> Result<([u8; MAX_ETHERNET_FRAME], usize), NetworkError> {
    const TCP_HEADER_BYTES: usize = 20;
    let tcp_length = TCP_HEADER_BYTES
        .checked_add(request.payload.len())
        .ok_or(NetworkError::FrameTooLarge)?;
    let ip_length = IPV4_HEADER_BYTES
        .checked_add(tcp_length)
        .ok_or(NetworkError::FrameTooLarge)?;
    let frame_length = ETHERNET_HEADER_BYTES
        .checked_add(ip_length)
        .ok_or(NetworkError::FrameTooLarge)?;
    if frame_length > MAX_ETHERNET_FRAME {
        return Err(NetworkError::FrameTooLarge);
    }
    let transmit_length = frame_length.max(MIN_ETHERNET_FRAME);
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    frame[0..6].copy_from_slice(&request.destination_mac);
    frame[6..12].copy_from_slice(&request.source_mac);
    put_be_u16(&mut frame, 12, ETHERTYPE_IPV4);

    let ip = ETHERNET_HEADER_BYTES;
    frame[ip] = 0x45;
    put_be_u16(
        &mut frame,
        ip + 2,
        u16::try_from(ip_length).map_err(|_| NetworkError::FrameTooLarge)?,
    );
    put_be_u16(
        &mut frame,
        ip + 4,
        query_packet_id(request.source_port, request.destination_port),
    );
    put_be_u16(&mut frame, ip + 6, 0x4000);
    frame[ip + 8] = 64;
    frame[ip + 9] = IP_PROTOCOL_TCP;
    frame[ip + 12..ip + 16].copy_from_slice(&request.source_address);
    frame[ip + 16..ip + 20].copy_from_slice(&request.destination_address);
    let ip_checksum = internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]);
    put_be_u16(&mut frame, ip + 10, ip_checksum);

    let tcp = ip + IPV4_HEADER_BYTES;
    put_be_u16(&mut frame, tcp, request.source_port);
    put_be_u16(&mut frame, tcp + 2, request.destination_port);
    put_be_u32(&mut frame, tcp + 4, request.sequence);
    put_be_u32(&mut frame, tcp + 8, request.acknowledgement);
    frame[tcp + 12] = (TCP_HEADER_BYTES as u8 / 4) << 4;
    frame[tcp + 13] = (request.flags & 0x3f) as u8;
    put_be_u16(&mut frame, tcp + 14, 0x4000);
    frame[tcp + TCP_HEADER_BYTES..tcp + TCP_HEADER_BYTES + request.payload.len()]
        .copy_from_slice(request.payload);
    let checksum = ipv4_transport_checksum(
        request.source_address,
        request.destination_address,
        IP_PROTOCOL_TCP,
        &frame[tcp..tcp + tcp_length],
    );
    put_be_u16(
        &mut frame,
        tcp + 16,
        if checksum == 0 { 0xffff } else { checksum },
    );
    Ok((frame, transmit_length))
}

fn wait_for_tcp_segment(
    device: &mut VirtioNet,
    expected_mac: [u8; 6],
    address: [u8; 4],
    remote: [u8; 4],
    local_port: u16,
    remote_port: u16,
    accept: fn(TcpSegment, &[u8]) -> bool,
) -> Result<TcpSegment, NetworkError> {
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    for _ in 0..PROTOCOL_TIMEOUT_SPINS {
        if let Some(length) = device.try_receive(&mut frame)?
            && let Ok(segment) = parse_tcp_segment(
                &frame[..length],
                expected_mac,
                address,
                remote,
                local_port,
                remote_port,
            )
        {
            if segment.flags & TCP_FLAG_RST != 0 {
                return Err(NetworkError::TcpRejected);
            }
            if accept(segment, &frame[..length]) {
                return Ok(segment);
            }
        }
        core::hint::spin_loop();
    }
    Err(NetworkError::TcpTimedOut)
}

fn wait_for_http_response(
    device: &mut VirtioNet,
    expected_mac: [u8; 6],
    address: [u8; 4],
    remote: [u8; 4],
    local_port: u16,
    remote_port: u16,
    expected_ack: u32,
) -> Result<HttpResponse, NetworkError> {
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    for _ in 0..PROTOCOL_TIMEOUT_SPINS {
        if let Some(length) = device.try_receive(&mut frame)?
            && let Ok(segment) = parse_tcp_segment(
                &frame[..length],
                expected_mac,
                address,
                remote,
                local_port,
                remote_port,
            )
        {
            if segment.flags & TCP_FLAG_RST != 0 {
                return Err(NetworkError::TcpRejected);
            }
            if segment.acknowledgement != expected_ack || segment.payload_len == 0 {
                continue;
            }
            let payload =
                &frame[segment.payload_offset..segment.payload_offset + segment.payload_len];
            if let Some(status) = parse_http_status(payload) {
                return Ok(HttpResponse {
                    segment,
                    proof: HttpProof {
                        status,
                        response_bytes: segment.payload_len,
                    },
                });
            }
        }
        core::hint::spin_loop();
    }
    Err(NetworkError::TcpTimedOut)
}

fn parse_tcp_segment(
    frame: &[u8],
    expected_mac: [u8; 6],
    address: [u8; 4],
    remote: [u8; 4],
    local_port: u16,
    remote_port: u16,
) -> Result<TcpSegment, NetworkError> {
    if frame.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + 20
        || frame.get(6..12) != Some(expected_mac.as_slice())
        || get_be_u16(frame, 12)? != ETHERTYPE_IPV4
    {
        return Err(NetworkError::PacketMalformed);
    }
    let ip = ETHERNET_HEADER_BYTES;
    let header_bytes = usize::from(frame[ip] & 0x0f) * 4;
    let total_bytes = usize::from(get_be_u16(frame, ip + 2)?);
    if frame[ip] >> 4 != 4
        || header_bytes < IPV4_HEADER_BYTES
        || total_bytes < header_bytes + 20
        || ip + total_bytes > frame.len()
        || internet_checksum(&frame[ip..ip + header_bytes]) != 0
        || frame[ip + 9] != IP_PROTOCOL_TCP
        || get_be_u16(frame, ip + 6)? & 0x3fff != 0
        || frame.get(ip + 12..ip + 16) != Some(remote.as_slice())
        || frame.get(ip + 16..ip + 20) != Some(address.as_slice())
    {
        return Err(NetworkError::PacketMalformed);
    }
    let tcp = ip + header_bytes;
    let tcp_length = total_bytes - header_bytes;
    let data_offset = usize::from(frame[tcp + 12] >> 4) * 4;
    if get_be_u16(frame, tcp)? != remote_port
        || get_be_u16(frame, tcp + 2)? != local_port
        || data_offset < 20
        || data_offset > tcp_length
    {
        return Err(NetworkError::PacketMalformed);
    }
    if ipv4_transport_checksum(
        remote,
        address,
        IP_PROTOCOL_TCP,
        &frame[tcp..tcp + tcp_length],
    ) != 0
    {
        return Err(NetworkError::PacketMalformed);
    }
    Ok(TcpSegment {
        sequence: get_be_u32(frame, tcp + 4)?,
        acknowledgement: get_be_u32(frame, tcp + 8)?,
        flags: u16::from(frame[tcp + 13]),
        payload_offset: tcp + data_offset,
        payload_len: tcp_length - data_offset,
    })
}

fn parse_tcp_listener_segment(
    frame: &[u8],
    local_mac: [u8; 6],
    local_address: [u8; 4],
    local_port: u16,
) -> Result<IncomingTcpSegment, NetworkError> {
    if frame.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + 20
        || frame.get(0..6) != Some(local_mac.as_slice())
        || get_be_u16(frame, 12)? != ETHERTYPE_IPV4
    {
        return Err(NetworkError::PacketMalformed);
    }
    let mut remote_mac = [0_u8; 6];
    remote_mac.copy_from_slice(&frame[6..12]);

    let ip = ETHERNET_HEADER_BYTES;
    let header_bytes = usize::from(frame[ip] & 0x0f) * 4;
    let total_bytes = usize::from(get_be_u16(frame, ip + 2)?);
    if frame[ip] >> 4 != 4
        || header_bytes < IPV4_HEADER_BYTES
        || total_bytes < header_bytes + 20
        || ip + total_bytes > frame.len()
        || internet_checksum(&frame[ip..ip + header_bytes]) != 0
        || frame[ip + 9] != IP_PROTOCOL_TCP
        || get_be_u16(frame, ip + 6)? & 0x3fff != 0
        || frame.get(ip + 16..ip + 20) != Some(local_address.as_slice())
    {
        return Err(NetworkError::PacketMalformed);
    }
    let remote_address = read_ipv4(frame, ip + 12)?;
    let tcp = ip + header_bytes;
    let tcp_length = total_bytes - header_bytes;
    let data_offset = usize::from(frame[tcp + 12] >> 4) * 4;
    if get_be_u16(frame, tcp + 2)? != local_port || data_offset < 20 || data_offset > tcp_length {
        return Err(NetworkError::PacketMalformed);
    }
    if ipv4_transport_checksum(
        remote_address,
        local_address,
        IP_PROTOCOL_TCP,
        &frame[tcp..tcp + tcp_length],
    ) != 0
    {
        return Err(NetworkError::PacketMalformed);
    }
    Ok(IncomingTcpSegment {
        remote_mac,
        remote_address,
        remote_port: get_be_u16(frame, tcp)?,
        segment: TcpSegment {
            sequence: get_be_u32(frame, tcp + 4)?,
            acknowledgement: get_be_u32(frame, tcp + 8)?,
            flags: u16::from(frame[tcp + 13]),
            payload_offset: tcp + data_offset,
            payload_len: tcp_length - data_offset,
        },
    })
}

fn http_request_for_host(host: &str) -> Result<&'static [u8], NetworkError> {
    if host != DNS_QUERY_NAME {
        return Err(NetworkError::HttpRejected);
    }
    Ok(HTTP_REQUEST)
}

fn is_supported_listener_request(payload: &[u8]) -> bool {
    payload.starts_with(b"GET /") || payload.starts_with(b"HEAD /")
}

fn parse_http_status(payload: &[u8]) -> Option<u16> {
    let prefix = payload.get(0..5)?;
    if prefix != b"HTTP/" {
        return None;
    }
    let status = payload.get(9..12)?;
    if !status.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = u16::from(status[0] - b'0') * 100
        + u16::from(status[1] - b'0') * 10
        + u16::from(status[2] - b'0');
    (100..600).contains(&value).then_some(value)
}

fn tcp_server_sequence(remote_address: [u8; 4], remote_port: u16) -> u32 {
    TCP_SERVER_INITIAL_SEQUENCE
        ^ u32::from_be_bytes(remote_address)
        ^ (u32::from(remote_port) << 16 | u32::from(remote_port))
}

fn build_udp_ipv4_frame(
    mac: [u8; 6],
    destination_mac: [u8; 6],
    address: [u8; 4],
    destination: [u8; 4],
    source_port: u16,
    destination_port: u16,
    payload: &[u8],
) -> Result<([u8; MAX_ETHERNET_FRAME], usize), NetworkError> {
    let udp_length = UDP_HEADER_BYTES
        .checked_add(payload.len())
        .ok_or(NetworkError::FrameTooLarge)?;
    let ip_length = IPV4_HEADER_BYTES
        .checked_add(udp_length)
        .ok_or(NetworkError::FrameTooLarge)?;
    let frame_length = ETHERNET_HEADER_BYTES
        .checked_add(ip_length)
        .ok_or(NetworkError::FrameTooLarge)?;
    if frame_length > MAX_ETHERNET_FRAME {
        return Err(NetworkError::FrameTooLarge);
    }
    let transmit_length = frame_length.max(MIN_ETHERNET_FRAME);
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    frame[0..6].copy_from_slice(&destination_mac);
    frame[6..12].copy_from_slice(&mac);
    put_be_u16(&mut frame, 12, ETHERTYPE_IPV4);

    let ip = ETHERNET_HEADER_BYTES;
    frame[ip] = 0x45;
    put_be_u16(
        &mut frame,
        ip + 2,
        u16::try_from(ip_length).map_err(|_| NetworkError::FrameTooLarge)?,
    );
    put_be_u16(
        &mut frame,
        ip + 4,
        query_packet_id(source_port, destination_port),
    );
    put_be_u16(&mut frame, ip + 6, 0x4000);
    frame[ip + 8] = 64;
    frame[ip + 9] = IP_PROTOCOL_UDP;
    frame[ip + 12..ip + 16].copy_from_slice(&address);
    frame[ip + 16..ip + 20].copy_from_slice(&destination);
    let ip_checksum = internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]);
    put_be_u16(&mut frame, ip + 10, ip_checksum);

    let udp = ip + IPV4_HEADER_BYTES;
    put_be_u16(&mut frame, udp, source_port);
    put_be_u16(&mut frame, udp + 2, destination_port);
    put_be_u16(
        &mut frame,
        udp + 4,
        u16::try_from(udp_length).map_err(|_| NetworkError::FrameTooLarge)?,
    );
    frame[udp + UDP_HEADER_BYTES..udp + UDP_HEADER_BYTES + payload.len()].copy_from_slice(payload);
    let checksum = ipv4_transport_checksum(
        address,
        destination,
        IP_PROTOCOL_UDP,
        &frame[udp..udp + udp_length],
    );
    put_be_u16(
        &mut frame,
        udp + 6,
        if checksum == 0 { 0xffff } else { checksum },
    );
    Ok((frame, transmit_length))
}

fn wait_for_dns_response(
    device: &mut VirtioNet,
    expected_mac: [u8; 6],
    address: [u8; 4],
    dns_server: [u8; 4],
    query_id: u16,
    name: &str,
) -> Result<[u8; 4], NetworkError> {
    let mut frame = [0_u8; MAX_ETHERNET_FRAME];
    for _ in 0..PROTOCOL_TIMEOUT_SPINS {
        if let Some(length) = device.try_receive(&mut frame)?
            && let Ok(answer) = parse_dns_response(
                &frame[..length],
                expected_mac,
                address,
                dns_server,
                query_id,
                name,
            )
        {
            return Ok(answer);
        }
        core::hint::spin_loop();
    }
    Err(NetworkError::DnsTimedOut)
}

fn parse_dns_response(
    frame: &[u8],
    expected_mac: [u8; 6],
    address: [u8; 4],
    dns_server: [u8; 4],
    query_id: u16,
    name: &str,
) -> Result<[u8; 4], NetworkError> {
    if frame.len() < ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + UDP_HEADER_BYTES + 12
        || frame.get(6..12) != Some(expected_mac.as_slice())
        || get_be_u16(frame, 12)? != ETHERTYPE_IPV4
    {
        return Err(NetworkError::PacketMalformed);
    }
    let ip = ETHERNET_HEADER_BYTES;
    let header_bytes = usize::from(frame[ip] & 0x0f) * 4;
    let total_bytes = usize::from(get_be_u16(frame, ip + 2)?);
    if frame[ip] >> 4 != 4
        || header_bytes < IPV4_HEADER_BYTES
        || total_bytes < header_bytes + UDP_HEADER_BYTES + 12
        || ip + total_bytes > frame.len()
        || internet_checksum(&frame[ip..ip + header_bytes]) != 0
        || frame[ip + 9] != IP_PROTOCOL_UDP
        || get_be_u16(frame, ip + 6)? & 0x3fff != 0
        || frame.get(ip + 12..ip + 16) != Some(dns_server.as_slice())
        || frame.get(ip + 16..ip + 20) != Some(address.as_slice())
    {
        return Err(NetworkError::PacketMalformed);
    }
    let udp = ip + header_bytes;
    let udp_length = usize::from(get_be_u16(frame, udp + 4)?);
    if get_be_u16(frame, udp)? != DNS_SERVER_PORT
        || get_be_u16(frame, udp + 2)? != DNS_CLIENT_PORT
        || udp_length < UDP_HEADER_BYTES + 12
        || udp + udp_length > ip + total_bytes
    {
        return Err(NetworkError::PacketMalformed);
    }
    let udp_checksum = get_be_u16(frame, udp + 6)?;
    if udp_checksum != 0
        && ipv4_transport_checksum(
            dns_server,
            address,
            IP_PROTOCOL_UDP,
            &frame[udp..udp + udp_length],
        ) != 0
    {
        return Err(NetworkError::PacketMalformed);
    }
    let dns = &frame[udp + UDP_HEADER_BYTES..udp + udp_length];
    parse_dns_message(dns, query_id, name)
}

fn parse_dns_message(dns: &[u8], query_id: u16, name: &str) -> Result<[u8; 4], NetworkError> {
    if dns.len() < 12
        || get_be_u16(dns, 0)? != query_id
        || get_be_u16(dns, 2)? & 0x8000 == 0
        || get_be_u16(dns, 2)? & 0x000f != 0
        || get_be_u16(dns, 4)? != 1
    {
        return Err(NetworkError::DnsRejected);
    }
    let answer_count = get_be_u16(dns, 6)?;
    let mut cursor = dns_name_matches(dns, 12, name)?;
    if get_be_u16(dns, cursor)? != 1 || get_be_u16(dns, cursor + 2)? != 1 {
        return Err(NetworkError::DnsRejected);
    }
    cursor += 4;
    for _ in 0..answer_count {
        cursor = skip_dns_name(dns, cursor)?;
        let answer_end = cursor
            .checked_add(10)
            .filter(|end| *end <= dns.len())
            .ok_or(NetworkError::PacketMalformed)?;
        let record_type = get_be_u16(dns, cursor)?;
        let record_class = get_be_u16(dns, cursor + 2)?;
        let data_len = usize::from(get_be_u16(dns, cursor + 8)?);
        cursor = answer_end;
        let data_end = cursor
            .checked_add(data_len)
            .filter(|end| *end <= dns.len())
            .ok_or(NetworkError::PacketMalformed)?;
        if record_type == 1 && record_class == 1 && data_len == 4 {
            return Ok([
                dns[cursor],
                dns[cursor + 1],
                dns[cursor + 2],
                dns[cursor + 3],
            ]);
        }
        cursor = data_end;
    }
    Err(NetworkError::DnsRejected)
}

fn write_dns_name(output: &mut [u8], cursor: &mut usize, name: &str) -> Result<(), NetworkError> {
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(NetworkError::DnsRejected);
        }
        let end = cursor
            .checked_add(1 + label.len())
            .filter(|end| *end < output.len())
            .ok_or(NetworkError::FrameTooLarge)?;
        output[*cursor] = label.len() as u8;
        *cursor += 1;
        output[*cursor..end].copy_from_slice(label.as_bytes());
        *cursor = end;
    }
    if *cursor >= output.len() {
        return Err(NetworkError::FrameTooLarge);
    }
    output[*cursor] = 0;
    *cursor += 1;
    Ok(())
}

fn dns_name_matches(dns: &[u8], mut cursor: usize, name: &str) -> Result<usize, NetworkError> {
    for label in name.split('.') {
        let length = usize::from(*dns.get(cursor).ok_or(NetworkError::PacketMalformed)?);
        cursor += 1;
        if length != label.len()
            || dns
                .get(cursor..cursor + length)
                .ok_or(NetworkError::PacketMalformed)?
                != label.as_bytes()
        {
            return Err(NetworkError::DnsRejected);
        }
        cursor += length;
    }
    if *dns.get(cursor).ok_or(NetworkError::PacketMalformed)? != 0 {
        return Err(NetworkError::DnsRejected);
    }
    Ok(cursor + 1)
}

fn skip_dns_name(dns: &[u8], mut cursor: usize) -> Result<usize, NetworkError> {
    loop {
        let length = *dns.get(cursor).ok_or(NetworkError::PacketMalformed)?;
        cursor += 1;
        if length & 0xc0 == 0xc0 {
            let _pointer_low = *dns.get(cursor).ok_or(NetworkError::PacketMalformed)?;
            return Ok(cursor + 1);
        }
        if length & 0xc0 != 0 {
            return Err(NetworkError::PacketMalformed);
        }
        if length == 0 {
            return Ok(cursor);
        }
        cursor = cursor
            .checked_add(usize::from(length))
            .filter(|cursor| *cursor <= dns.len())
            .ok_or(NetworkError::PacketMalformed)?;
    }
}

fn route_next_hop(
    address: [u8; 4],
    subnet_mask: [u8; 4],
    gateway: [u8; 4],
    destination: [u8; 4],
) -> [u8; 4] {
    if same_subnet(address, destination, subnet_mask) {
        destination
    } else {
        gateway
    }
}

fn dns_next_hop(
    address: [u8; 4],
    subnet_mask: [u8; 4],
    gateway: [u8; 4],
    dns_server: [u8; 4],
) -> [u8; 4] {
    if dns_server == default_dns_server(address, gateway) {
        gateway
    } else {
        route_next_hop(address, subnet_mask, gateway, dns_server)
    }
}

fn same_subnet(left: [u8; 4], right: [u8; 4], mask: [u8; 4]) -> bool {
    left.iter()
        .zip(right.iter())
        .zip(mask.iter())
        .all(|((left, right), mask)| left & mask == right & mask)
}

fn default_dns_server(address: [u8; 4], gateway: [u8; 4]) -> [u8; 4] {
    let mut dns = gateway;
    dns[3] = gateway[3].saturating_add(1);
    if dns == address { gateway } else { dns }
}

fn query_packet_id(source_port: u16, destination_port: u16) -> u16 {
    source_port ^ destination_port
}

fn write_option(output: &mut [u8], cursor: &mut usize, code: u8, value: &[u8]) {
    output[*cursor] = code;
    output[*cursor + 1] = value.len() as u8;
    output[*cursor + 2..*cursor + 2 + value.len()].copy_from_slice(value);
    *cursor += 2 + value.len();
}

fn read_ipv4(input: &[u8], offset: usize) -> Result<[u8; 4], NetworkError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or(NetworkError::PacketMalformed)?;
    Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn put_be_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn put_be_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn get_be_u16(input: &[u8], offset: usize) -> Result<u16, NetworkError> {
    let bytes = input
        .get(offset..offset + 2)
        .ok_or(NetworkError::PacketMalformed)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn get_be_u32(input: &[u8], offset: usize) -> Result<u32, NetworkError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or(NetworkError::PacketMalformed)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    finish_checksum(checksum_sum(bytes))
}

fn ipv4_transport_checksum(
    source: [u8; 4],
    destination: [u8; 4],
    protocol: u8,
    segment: &[u8],
) -> u16 {
    let Ok(length) = u16::try_from(segment.len()) else {
        return 1;
    };
    let mut pseudo_header = [0_u8; 12];
    pseudo_header[0..4].copy_from_slice(&source);
    pseudo_header[4..8].copy_from_slice(&destination);
    pseudo_header[9] = protocol;
    pseudo_header[10..12].copy_from_slice(&length.to_be_bytes());
    finish_checksum(checksum_sum(&pseudo_header).wrapping_add(checksum_sum(segment)))
}

fn checksum_sum(bytes: &[u8]) -> u32 {
    let mut sum = 0_u32;
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum = sum.wrapping_add(u32::from(u16::from_be_bytes([chunk[0], chunk[1]])));
    }
    if let [last] = chunks.remainder() {
        sum = sum.wrapping_add(u32::from(*last) << 8);
    }
    sum
}

fn finish_checksum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn allocate_dma_page() -> Result<(u64, *mut u8), NetworkError> {
    let physical = memory::allocate_page().ok_or(NetworkError::DmaAddressUnavailable)?;
    let virtual_address = vm::physical_to_high_half(physical)
        .map(|address| address as *mut u8)
        .ok_or(NetworkError::DmaAddressUnavailable)?;
    unsafe {
        core::ptr::write_bytes(virtual_address, 0, DMA_BUFFER_BYTES);
    }
    Ok((physical, virtual_address))
}

fn virtqueue_layout(size: u16) -> (usize, usize) {
    let descriptor_bytes = core::mem::size_of::<VirtqDescriptor>() * usize::from(size);
    let available_bytes = 6 + 2 * usize::from(size);
    let used_offset = align_up(descriptor_bytes + available_bytes, 4096);
    let used_bytes = 6 + 8 * usize::from(size);
    (used_offset + used_bytes, used_offset)
}

fn find_legacy_virtio_net() -> Option<hardware::PciAddress> {
    hardware::find_legacy_virtio_device(hardware::VIRTIO_NET_LEGACY_DEVICE_ID)
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
    fn ipv4_header_checksum_round_trips() {
        let frame = build_dhcp_frame([0x52, 0x54, 0, 0x12, 0x34, 0x56], DHCP_DISCOVER, None, None);
        let ip = ETHERNET_HEADER_BYTES;
        assert_eq!(internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]), 0);
    }

    #[test]
    fn dhcp_discover_has_valid_wire_fields() {
        let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let frame = build_dhcp_frame(mac, DHCP_DISCOVER, None, None);
        assert_eq!(get_be_u16(&frame, 12).unwrap(), ETHERTYPE_IPV4);
        let dhcp = ETHERNET_HEADER_BYTES + IPV4_HEADER_BYTES + UDP_HEADER_BYTES;
        assert_eq!(get_be_u32(&frame, dhcp + 4).unwrap(), DHCP_XID);
        assert_eq!(&frame[dhcp + 28..dhcp + 34], &mac);
        assert!(
            frame[dhcp + 240..]
                .windows(3)
                .any(|option| option == [53, 1, DHCP_DISCOVER])
        );
    }

    #[test]
    fn arp_reply_parser_rejects_wrong_sender() {
        let host = [10, 0, 2, 15];
        let gateway = [10, 0, 2, 2];
        let mut frame = build_arp_request([0x52, 0x54, 0, 1, 2, 3], host, gateway);
        put_be_u16(&mut frame, 20, 2);
        frame[22..28].copy_from_slice(&[0x52, 0x55, 10, 0, 2, 2]);
        frame[28..32].copy_from_slice(&gateway);
        frame[38..42].copy_from_slice(&host);
        assert!(parse_arp_reply(&frame, host, gateway).is_ok());
        assert!(parse_arp_reply(&frame, host, [10, 0, 2, 3]).is_err());
    }

    #[test]
    fn live_arp_responder_returns_its_configured_identity() {
        let local_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let local_address = [10, 0, 2, 15];
        let remote_address = [10, 0, 2, 2];
        let request = build_arp_request(remote_mac, remote_address, local_address);
        let reply = build_arp_reply(&request, local_mac, local_address).unwrap();
        assert_eq!(
            parse_arp_reply(&reply, remote_address, local_address).unwrap(),
            local_mac
        );
    }

    #[test]
    fn icmp_echo_request_has_valid_ip_and_icmp_checksums() {
        let frame = build_icmp_echo_request(
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            [0x52, 0x55, 10, 0, 2, 2],
            [10, 0, 2, 15],
            [10, 0, 2, 2],
        );
        let ip = ETHERNET_HEADER_BYTES;
        assert_eq!(internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]), 0);
        let total = usize::from(get_be_u16(&frame, ip + 2).unwrap());
        assert_eq!(
            internet_checksum(&frame[ip + IPV4_HEADER_BYTES..ip + total]),
            0
        );
    }

    #[test]
    fn dns_query_frame_has_valid_udp_checksum_and_question() {
        let (frame, length) = build_dns_query_frame(
            [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            [0x52, 0x55, 10, 0, 2, 3],
            [10, 0, 2, 15],
            [10, 0, 2, 3],
            DNS_QUERY_ID,
            DNS_QUERY_NAME,
        )
        .unwrap();
        let frame = &frame[..length];
        let ip = ETHERNET_HEADER_BYTES;
        let udp = ip + IPV4_HEADER_BYTES;
        let total = usize::from(get_be_u16(frame, ip + 2).unwrap());
        let udp_length = usize::from(get_be_u16(frame, udp + 4).unwrap());
        assert_eq!(internet_checksum(&frame[ip..ip + IPV4_HEADER_BYTES]), 0);
        assert_eq!(
            ipv4_transport_checksum(
                [10, 0, 2, 15],
                [10, 0, 2, 3],
                IP_PROTOCOL_UDP,
                &frame[udp..udp + udp_length],
            ),
            0
        );
        let dns = udp + UDP_HEADER_BYTES;
        assert_eq!(get_be_u16(frame, dns).unwrap(), DNS_QUERY_ID);
        assert!(dns_name_matches(frame, dns + 12, DNS_QUERY_NAME).is_ok());
        assert!(total >= IPV4_HEADER_BYTES + UDP_HEADER_BYTES + 12);
    }

    #[test]
    fn dns_response_parser_extracts_a_record() {
        let client_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let dns_mac = [0x52, 0x55, 10, 0, 2, 3];
        let client = [10, 0, 2, 15];
        let dns_server = [10, 0, 2, 3];
        let mut payload = [0_u8; 256];
        put_be_u16(&mut payload, 0, DNS_QUERY_ID);
        put_be_u16(&mut payload, 2, 0x8180);
        put_be_u16(&mut payload, 4, 1);
        put_be_u16(&mut payload, 6, 1);
        let mut cursor = 12;
        write_dns_name(&mut payload, &mut cursor, DNS_QUERY_NAME).unwrap();
        put_be_u16(&mut payload, cursor, 1);
        put_be_u16(&mut payload, cursor + 2, 1);
        cursor += 4;
        payload[cursor] = 0xc0;
        payload[cursor + 1] = 0x0c;
        cursor += 2;
        put_be_u16(&mut payload, cursor, 1);
        put_be_u16(&mut payload, cursor + 2, 1);
        put_be_u32(&mut payload, cursor + 4, 60);
        put_be_u16(&mut payload, cursor + 8, 4);
        cursor += 10;
        payload[cursor..cursor + 4].copy_from_slice(&[93, 184, 216, 34]);
        cursor += 4;
        let (frame, length) = build_udp_ipv4_frame(
            dns_mac,
            client_mac,
            dns_server,
            client,
            DNS_SERVER_PORT,
            DNS_CLIENT_PORT,
            &payload[..cursor],
        )
        .unwrap();
        assert_eq!(
            parse_dns_response(
                &frame[..length],
                dns_mac,
                client,
                dns_server,
                DNS_QUERY_ID,
                DNS_QUERY_NAME,
            )
            .unwrap(),
            [93, 184, 216, 34]
        );
    }

    #[test]
    fn tcp_segment_parser_validates_checksum_and_flags() {
        let client_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let client = [10, 0, 2, 15];
        let remote = [93, 184, 216, 34];
        let (frame, length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: client_mac,
            source_address: remote,
            destination_address: client,
            source_port: HTTP_SERVER_PORT,
            destination_port: HTTP_CLIENT_PORT,
            sequence: 0x1020_3040,
            acknowledgement: TCP_INITIAL_SEQUENCE + 1,
            flags: TCP_FLAG_SYN | TCP_FLAG_ACK,
            payload: &[],
        })
        .unwrap();
        let segment = parse_tcp_segment(
            &frame[..length],
            remote_mac,
            client,
            remote,
            HTTP_CLIENT_PORT,
            HTTP_SERVER_PORT,
        )
        .unwrap();
        assert_eq!(segment.sequence, 0x1020_3040);
        assert_eq!(segment.acknowledgement, TCP_INITIAL_SEQUENCE + 1);
        assert_eq!(
            segment.flags & (TCP_FLAG_SYN | TCP_FLAG_ACK),
            TCP_FLAG_SYN | TCP_FLAG_ACK
        );
    }

    #[test]
    fn http_status_parser_accepts_real_status_line() {
        assert_eq!(parse_http_status(b"HTTP/1.1 301 Moved\r\n"), Some(301));
        assert_eq!(parse_http_status(b"not-http"), None);
        let body = HTTP_LISTENER_RESPONSE
            .split(|byte| *byte == b'\n')
            .rev()
            .nth(1)
            .unwrap();
        assert_eq!(body, b"codexOS listener online");
        assert_eq!(body.len() + 1, 24);
    }

    #[test]
    fn live_icmp_responder_returns_a_valid_echo_reply() {
        let local_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let local_address = [10, 0, 2, 15];
        let remote_address = [10, 0, 2, 2];
        let request = build_icmp_echo_request(remote_mac, local_mac, remote_address, local_address);
        let (reply, length) = build_icmp_echo_reply(&request, local_mac, local_address).unwrap();
        assert!(
            parse_icmp_echo_reply(&reply[..length], local_mac, remote_address, local_address)
                .is_ok()
        );
    }

    #[test]
    fn tcp_listener_accepts_a_host_http_connection() {
        let local_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let local_address = [10, 0, 2, 15];
        let remote_address = [10, 0, 2, 2];
        let remote_port = 55000;
        let remote_sequence = 0x1020_3040;
        let mut listener = TcpListener::new(TCP_LISTENER_PORT);

        let (syn, syn_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: remote_sequence,
            acknowledgement: 0,
            flags: TCP_FLAG_SYN,
            payload: &[],
        })
        .unwrap();
        let syn_ack = listener
            .handle_frame(&syn[..syn_length], local_mac, local_address)
            .unwrap()
            .unwrap();
        let accepted = syn_ack.event.unwrap();
        assert_eq!(accepted.kind, TcpServerEventKind::Accepted);
        assert_eq!(accepted.local_port, TCP_LISTENER_PORT);
        assert_eq!(accepted.remote_address, remote_address);
        assert_eq!(accepted.remote_port, remote_port);
        let syn_ack_segment = parse_tcp_segment(
            &syn_ack.frame[..syn_ack.length],
            local_mac,
            remote_address,
            local_address,
            remote_port,
            TCP_LISTENER_PORT,
        )
        .unwrap();
        assert_eq!(
            syn_ack_segment.flags & (TCP_FLAG_SYN | TCP_FLAG_ACK),
            TCP_FLAG_SYN | TCP_FLAG_ACK
        );
        assert_eq!(syn_ack_segment.acknowledgement, remote_sequence + 1);

        let client_next = remote_sequence + 1;
        let server_next = syn_ack_segment.sequence + 1;
        let (ack, ack_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: client_next,
            acknowledgement: server_next,
            flags: TCP_FLAG_ACK,
            payload: &[],
        })
        .unwrap();
        assert!(
            listener
                .handle_frame(&ack[..ack_length], local_mac, local_address)
                .unwrap()
                .is_none()
        );

        let request = b"GET / HTTP/1.1\r\nHost: codexos.local\r\nConnection: close\r\n\r\n";
        let (get, get_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: client_next,
            acknowledgement: server_next,
            flags: TCP_FLAG_PSH | TCP_FLAG_ACK,
            payload: request,
        })
        .unwrap();
        let response = listener
            .handle_frame(&get[..get_length], local_mac, local_address)
            .unwrap()
            .unwrap();
        let event = response.event.unwrap();
        assert_eq!(event.kind, TcpServerEventKind::Served);
        assert_eq!(event.local_port, TCP_LISTENER_PORT);
        assert_eq!(event.remote_address, remote_address);
        assert_eq!(event.remote_port, remote_port);
        assert_eq!(event.request_bytes, request.len());
        assert_eq!(event.response_bytes, HTTP_LISTENER_RESPONSE.len());
        assert_eq!(event.connection_count, 1);

        let response_segment = parse_tcp_segment(
            &response.frame[..response.length],
            local_mac,
            remote_address,
            local_address,
            remote_port,
            TCP_LISTENER_PORT,
        )
        .unwrap();
        assert_eq!(
            response_segment.flags & (TCP_FLAG_PSH | TCP_FLAG_ACK | TCP_FLAG_FIN),
            TCP_FLAG_PSH | TCP_FLAG_ACK | TCP_FLAG_FIN
        );
        let payload = &response.frame[response_segment.payload_offset
            ..response_segment.payload_offset + response_segment.payload_len];
        assert_eq!(parse_http_status(payload), Some(200));
        assert!(payload.ends_with(b"codexOS listener online\n"));
    }

    #[test]
    fn tcp_listener_keeps_concurrent_peers_isolated() {
        let local_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let local_address = [10, 0, 2, 15];
        let remote_address = [10, 0, 2, 2];
        let mut listener = TcpListener::new(TCP_LISTENER_PORT);
        let first = complete_listener_handshake(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            55000,
            0x1020_3040,
        );
        let second = complete_listener_handshake(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            55001,
            0x5060_7080,
        );
        assert_eq!(
            listener
                .connections
                .iter()
                .filter(|connection| connection.is_some())
                .count(),
            2
        );

        let first_event = send_listener_request(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            55000,
            first,
        );
        assert_eq!(first_event.connection_count, 1);
        assert!(
            listener
                .connections
                .iter()
                .any(|entry| { entry.is_some_and(|connection| connection.remote_port == 55001) })
        );

        let second_event = send_listener_request(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            55001,
            second,
        );
        assert_eq!(second_event.connection_count, 2);
        assert_eq!(
            listener
                .connections
                .iter()
                .filter(|connection| connection.is_some())
                .count(),
            2
        );
        assert!(
            listener
                .connections
                .iter()
                .flatten()
                .all(|connection| { connection.phase == TcpServerPhase::FinWait1 })
        );
    }

    #[test]
    fn tcp_listener_retransmits_response_and_completes_fin_handshake() {
        let local_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let local_address = [10, 0, 2, 15];
        let remote_address = [10, 0, 2, 2];
        let remote_port = 55000;
        let mut listener = TcpListener::new(TCP_LISTENER_PORT);
        let sequence = complete_listener_handshake(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            remote_port,
            0x1020_3040,
        );
        let response = send_listener_request_action(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            remote_port,
            sequence,
        );
        assert_eq!(response.event.unwrap().connection_count, 1);
        let retransmission = send_listener_request_action(
            &mut listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            remote_port,
            sequence,
        );
        assert!(retransmission.event.is_none());
        assert_eq!(
            &response.frame[..response.length],
            &retransmission.frame[..retransmission.length]
        );
        assert_eq!(listener.served_connections, 1);

        let connection_index = listener
            .connections
            .iter()
            .position(Option::is_some)
            .unwrap();
        let connection = listener.connections[connection_index].unwrap();
        assert_eq!(connection.phase, TcpServerPhase::FinWait1);
        let (fin_ack, fin_ack_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: connection.remote_next,
            acknowledgement: connection.local_next,
            flags: TCP_FLAG_ACK,
            payload: &[],
        })
        .unwrap();
        assert!(
            listener
                .handle_frame(&fin_ack[..fin_ack_length], local_mac, local_address)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            listener.connections[connection_index].unwrap().phase,
            TcpServerPhase::FinWait2
        );

        let (fin, fin_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: connection.remote_next,
            acknowledgement: connection.local_next,
            flags: TCP_FLAG_FIN | TCP_FLAG_ACK,
            payload: &[],
        })
        .unwrap();
        let final_ack = listener
            .handle_frame(&fin[..fin_length], local_mac, local_address)
            .unwrap()
            .unwrap();
        let final_ack_segment = parse_tcp_segment(
            &final_ack.frame[..final_ack.length],
            local_mac,
            remote_address,
            local_address,
            remote_port,
            TCP_LISTENER_PORT,
        )
        .unwrap();
        assert_eq!(final_ack_segment.flags, TCP_FLAG_ACK);
        assert_eq!(
            final_ack_segment.acknowledgement,
            connection.remote_next.wrapping_add(1)
        );
        assert!(listener.connections[connection_index].is_none());
        assert_eq!(listener.closed_connections, 1);
    }

    #[test]
    fn tcp_listener_refuses_excess_connections_without_eviction() {
        let local_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let remote_mac = [0x52, 0x55, 10, 0, 2, 2];
        let local_address = [10, 0, 2, 15];
        let remote_address = [10, 0, 2, 2];
        let mut listener = TcpListener::new(TCP_LISTENER_PORT);
        for index in 0..TCP_LISTENER_CAPACITY {
            complete_listener_handshake(
                &mut listener,
                local_mac,
                remote_mac,
                local_address,
                remote_address,
                55000 + index as u16,
                0x1020_3040 + index as u32 * 0x100,
            );
        }

        let (syn, syn_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: 56000,
            destination_port: TCP_LISTENER_PORT,
            sequence: 0x90a0_b0c0,
            acknowledgement: 0,
            flags: TCP_FLAG_SYN,
            payload: &[],
        })
        .unwrap();
        let refusal = listener
            .handle_frame(&syn[..syn_length], local_mac, local_address)
            .unwrap()
            .unwrap();
        let refusal_segment = parse_tcp_segment(
            &refusal.frame[..refusal.length],
            local_mac,
            remote_address,
            local_address,
            56000,
            TCP_LISTENER_PORT,
        )
        .unwrap();
        assert_eq!(
            refusal_segment.flags & (TCP_FLAG_RST | TCP_FLAG_ACK),
            TCP_FLAG_RST | TCP_FLAG_ACK
        );
        assert_eq!(
            listener
                .connections
                .iter()
                .filter(|connection| connection.is_some())
                .count(),
            TCP_LISTENER_CAPACITY
        );
        assert!(
            listener
                .connections
                .iter()
                .any(|entry| { entry.is_some_and(|connection| connection.remote_port == 55000) })
        );
        assert_eq!(
            listener.expire_idle_connections(TCP_LISTENER_IDLE_TIMEOUT_TICKS - 1),
            0
        );
        assert_eq!(
            listener.expire_idle_connections(TCP_LISTENER_IDLE_TIMEOUT_TICKS),
            TCP_LISTENER_CAPACITY
        );
        let accepted = listener
            .handle_frame_at(
                &syn[..syn_length],
                local_mac,
                local_address,
                TCP_LISTENER_IDLE_TIMEOUT_TICKS,
            )
            .unwrap()
            .unwrap();
        assert_eq!(accepted.event.unwrap().kind, TcpServerEventKind::Accepted);
    }

    fn complete_listener_handshake(
        listener: &mut TcpListener,
        local_mac: [u8; 6],
        remote_mac: [u8; 6],
        local_address: [u8; 4],
        remote_address: [u8; 4],
        remote_port: u16,
        remote_sequence: u32,
    ) -> (u32, u32) {
        let (syn, syn_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: remote_sequence,
            acknowledgement: 0,
            flags: TCP_FLAG_SYN,
            payload: &[],
        })
        .unwrap();
        let syn_ack = listener
            .handle_frame(&syn[..syn_length], local_mac, local_address)
            .unwrap()
            .unwrap();
        assert_eq!(syn_ack.event.unwrap().kind, TcpServerEventKind::Accepted);
        let syn_ack_segment = parse_tcp_segment(
            &syn_ack.frame[..syn_ack.length],
            local_mac,
            remote_address,
            local_address,
            remote_port,
            TCP_LISTENER_PORT,
        )
        .unwrap();
        let client_next = remote_sequence.wrapping_add(1);
        let server_next = syn_ack_segment.sequence.wrapping_add(1);
        let (ack, ack_length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: client_next,
            acknowledgement: server_next,
            flags: TCP_FLAG_ACK,
            payload: &[],
        })
        .unwrap();
        assert!(
            listener
                .handle_frame(&ack[..ack_length], local_mac, local_address)
                .unwrap()
                .is_none()
        );
        (client_next, server_next)
    }

    fn send_listener_request(
        listener: &mut TcpListener,
        local_mac: [u8; 6],
        remote_mac: [u8; 6],
        local_address: [u8; 4],
        remote_address: [u8; 4],
        remote_port: u16,
        sequence: (u32, u32),
    ) -> TcpServerEvent {
        send_listener_request_action(
            listener,
            local_mac,
            remote_mac,
            local_address,
            remote_address,
            remote_port,
            sequence,
        )
        .event
        .unwrap()
    }

    fn send_listener_request_action(
        listener: &mut TcpListener,
        local_mac: [u8; 6],
        remote_mac: [u8; 6],
        local_address: [u8; 4],
        remote_address: [u8; 4],
        remote_port: u16,
        sequence: (u32, u32),
    ) -> TcpServerAction {
        let request = b"GET / HTTP/1.1\r\nHost: codexos.local\r\nConnection: close\r\n\r\n";
        let (frame, length) = build_tcp_ipv4_frame(TcpFrameRequest {
            source_mac: remote_mac,
            destination_mac: local_mac,
            source_address: remote_address,
            destination_address: local_address,
            source_port: remote_port,
            destination_port: TCP_LISTENER_PORT,
            sequence: sequence.0,
            acknowledgement: sequence.1,
            flags: TCP_FLAG_PSH | TCP_FLAG_ACK,
            payload: request,
        })
        .unwrap();
        listener
            .handle_frame(&frame[..length], local_mac, local_address)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn virtqueue_layout_keeps_used_ring_page_aligned() {
        let (bytes, used) = virtqueue_layout(256);
        assert_eq!(used, 8192);
        assert!(bytes <= 12 * 1024);
    }
}
