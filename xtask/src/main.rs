use anyhow::{Context, Result, bail};
use codex_release::{BootState, KEY_ID_BYTES, PUBLIC_KEY_BYTES, SystemSlot, UpdateManifest};
use ed25519_dalek::{Signature, Signer, SigningKey};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

const EFI_TARGET: &str = "x86_64-unknown-uefi";
const KERNEL_TARGET: &str = "x86_64-unknown-none";
const LOADER_BIN: &str = "uefi-loader.efi";
const KERNEL_BIN: &str = "kernel-image";
const IMAGE_SIZE_BYTES: u64 = 256 * 1024 * 1024;
const DATA_IMAGE_SIZE_BYTES: u64 = 64 * 1024 * 1024;
const DEVELOPMENT_SIGNING_KEY: &str = "codexos-development-signing-key.bin";
const NETWORK_LISTENER_GUEST_PORT: u16 = 8080;
const DISK_SECTOR_BYTES: u64 = 512;
const GPT_ENTRY_COUNT: u32 = 128;
const GPT_ENTRY_BYTES: u32 = 128;
const GPT_PRIMARY_HEADER_LBA: u64 = 1;
const GPT_PRIMARY_ENTRIES_LBA: u64 = 2;
const GPT_ESP_FIRST_LBA: u64 = 2048;
const EFI_SYSTEM_PARTITION_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const CODEXOS_DISK_GUID: [u8; 16] = [
    0x40, 0x58, 0x44, 0x43, 0x65, 0x34, 0x4f, 0x53, 0x95, 0x20, 0x26, 0x06, 0x27, 0x00, 0x00, 0x01,
];
const CODEXOS_ESP_GUID: [u8; 16] = [
    0x40, 0x58, 0x44, 0x43, 0x65, 0x34, 0x4f, 0x53, 0x95, 0x20, 0x26, 0x06, 0x27, 0x00, 0x00, 0x02,
];

#[derive(Clone, Copy)]
enum LoaderMode {
    Interactive,
    Handoff,
    Chainload,
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("build") => {
            build_loader(false, LoaderMode::Interactive)?;
            build_kernel_image(true)?;
        }
        Some("build-handoff") => {
            build_loader(false, LoaderMode::Handoff)?;
            build_kernel_image(true)?;
        }
        Some("build-chainload") => {
            build_loader(false, LoaderMode::Chainload)?;
            build_kernel_image(true)?;
        }
        Some("image") => {
            let loader = build_loader(true, LoaderMode::Interactive)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Interactive)?;
            println!("disk image ready: {}", image.display());
        }
        Some("image-handoff") => {
            let loader = build_loader(true, LoaderMode::Handoff)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Handoff)?;
            println!("disk image ready: {}", image.display());
        }
        Some("image-chainload") => {
            let loader = build_loader(true, LoaderMode::Chainload)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Chainload)?;
            println!("disk image ready: {}", image.display());
        }
        Some("image-gpt-chainload") => {
            let loader = build_loader(true, LoaderMode::Chainload)?;
            let kernel = build_kernel_image(true)?;
            let image = make_gpt_image(&loader, &kernel, LoaderMode::Chainload)?;
            println!("gpt disk image ready: {}", image.display());
        }
        Some("run") => {
            let loader = build_loader(false, LoaderMode::Interactive)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Interactive)?;
            run_qemu(&image, false)?;
        }
        Some("handoff") => {
            let loader = build_loader(false, LoaderMode::Handoff)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Handoff)?;
            run_qemu(&image, false)?;
        }
        Some("chainload") => {
            let loader = build_loader(false, LoaderMode::Chainload)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Chainload)?;
            run_qemu(&image, false)?;
        }
        Some("debug") => {
            let loader = build_loader(false, LoaderMode::Interactive)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Interactive)?;
            run_qemu(&image, true)?;
        }
        Some("smoke") => {
            let loader = build_loader(true, LoaderMode::Interactive)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Interactive)?;
            smoke_qemu(&image, LoaderMode::Interactive)?;
        }
        Some("smoke-handoff") => {
            let loader = build_loader(true, LoaderMode::Handoff)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Handoff)?;
            smoke_qemu(&image, LoaderMode::Handoff)?;
        }
        Some("smoke-chainload") => {
            let loader = build_loader(true, LoaderMode::Chainload)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Chainload)?;
            smoke_qemu(&image, LoaderMode::Chainload)?;
        }
        Some("smoke-security") => smoke_security()?,
        Some("smoke-trust-rotation") => smoke_trust_rotation()?,
        Some("smoke-network-listener") => smoke_network_listener()?,
        Some("smoke-pointer-input") => smoke_pointer_input()?,
        Some("smoke-gpt-esp") => smoke_gpt_esp()?,
        Some("smoke-recovery") => smoke_recovery()?,
        Some("smoke-bootstate-recovery") => smoke_boot_state_recovery()?,
        Some("smoke-install") => smoke_install_and_update()?,
        Some("smoke-gpt-install") => smoke_gpt_install_and_update()?,
        Some("install") => {
            let destination = args
                .next()
                .context("install requires a destination image path")?;
            install_system(Path::new(&destination))?;
        }
        Some("install-gpt") => {
            let destination = args
                .next()
                .context("install-gpt requires a destination image path")?;
            install_gpt_system(Path::new(&destination))?;
        }
        Some("apply-update") => {
            let image = args
                .next()
                .context("apply-update requires an installed image path")?;
            apply_signed_update(Path::new(&image))?;
        }
        Some("release-image") => {
            let destination = args
                .next()
                .context("release-image requires a destination image path")?;
            build_production_release(Path::new(&destination))?;
        }
        Some("release-gpt-image") => {
            let destination = args
                .next()
                .context("release-gpt-image requires a destination image path")?;
            build_production_gpt_release(Path::new(&destination))?;
        }
        Some("derive-public-key") => {
            let seed = args
                .next()
                .context("derive-public-key requires a 32-byte signing seed path")?;
            let output = args
                .next()
                .context("derive-public-key requires a destination public-key path")?;
            derive_public_key(Path::new(&seed), Path::new(&output))?;
        }
        Some("env") => print_env()?,
        _ => print_help(),
    }

    Ok(())
}

fn print_help() {
    println!("codexOS xtask");
    println!("  cargo xtask build          - build the interactive UEFI loader plus kernel ELF");
    println!("  cargo xtask build-handoff  - build the handoff UEFI loader plus kernel ELF");
    println!("  cargo xtask build-chainload - build the handoff chainloader plus kernel ELF");
    println!("  cargo xtask image          - pack an optimized interactive FAT disk image");
    println!("  cargo xtask image-handoff  - pack an optimized handoff FAT disk image");
    println!("  cargo xtask image-chainload - pack an optimized chainload FAT disk image");
    println!("  cargo xtask image-gpt-chainload - pack a GPT disk with an EFI System Partition");
    println!("  cargo xtask run            - launch the interactive desktop in QEMU");
    println!("  cargo xtask handoff        - launch the handoff desktop in QEMU");
    println!("  cargo xtask chainload      - launch the standalone kernel chainload path in QEMU");
    println!("  cargo xtask debug          - launch QEMU paused with a gdb stub on :1234");
    println!("  cargo xtask smoke          - verify the interactive kernel log");
    println!("  cargo xtask smoke-handoff  - verify the post-EBS kernel log");
    println!("  cargo xtask smoke-chainload - verify the standalone kernel chainload log");
    println!("  cargo xtask smoke-security - reject kernel tampering and release rollback");
    println!(
        "  cargo xtask smoke-trust-rotation - rotate the release trust root and reject the old root"
    );
    println!(
        "  cargo xtask smoke-network-listener - connect from the host into the kernel TCP listener"
    );
    println!("  cargo xtask smoke-pointer-input - inject PS/2 pointer movement through QEMU");
    println!("  cargo xtask smoke-gpt-esp - boot the GPT/EFI System Partition disk layout");
    println!("  cargo xtask smoke-recovery - boot the signed fallback system slot");
    println!(
        "  cargo xtask smoke-bootstate-recovery - recover when both boot-state records are damaged"
    );
    println!("  cargo xtask smoke-install - install, update, and boot an A/B image");
    println!("  cargo xtask smoke-gpt-install - install, update, and boot a GPT/ESP A/B image");
    println!("  cargo xtask install <path> - install and readback-verify an A/B system image");
    println!("  cargo xtask install-gpt <path> - install and verify a GPT/ESP A/B system image");
    println!("  cargo xtask apply-update <path> - atomically update the inactive signed slot");
    println!("  cargo xtask release-image <path> - build with an external production signing key");
    println!("  cargo xtask release-gpt-image <path> - build a production GPT/ESP release image");
    println!("  cargo xtask derive-public-key <seed> <public-key> - export a release public key");
    println!("  cargo xtask env            - print discovered tool paths");
}

fn build_loader(release: bool, mode: LoaderMode) -> Result<PathBuf> {
    let signing_identity = load_signing_identity()?;
    build_loader_with_trust_key(release, mode, &signing_identity.verifying_key)
}

fn build_loader_with_trust_key(
    release: bool,
    mode: LoaderMode,
    trust_key: &ed25519_dalek::VerifyingKey,
) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command.arg("build");
    command.arg("-p").arg("uefi-loader");
    command.arg("--target").arg(EFI_TARGET);
    command.env(
        "CODEXOS_TRUSTED_PUBLIC_KEY_HEX",
        hex_encode(trust_key.as_bytes()),
    );
    match mode {
        LoaderMode::Interactive => {}
        LoaderMode::Handoff => {
            command.arg("--features").arg("handoff");
        }
        LoaderMode::Chainload => {
            command.arg("--features").arg("handoff,chainload");
        }
    }
    if release {
        command.arg("--release");
    }

    let context = match mode {
        LoaderMode::Interactive => "building the UEFI loader",
        LoaderMode::Handoff => "building the handoff UEFI loader",
        LoaderMode::Chainload => "building the chainload UEFI loader",
    };
    run(command, context)?;

    let profile = if release { "release" } else { "debug" };
    let loader = workspace_root()
        .join("target")
        .join(EFI_TARGET)
        .join(profile)
        .join(LOADER_BIN);

    if !loader.exists() {
        bail!("expected loader binary at {}", loader.display());
    }

    let staged_loader = workspace_root().join("build").join(match mode {
        LoaderMode::Interactive => "uefi-loader-interactive.efi",
        LoaderMode::Handoff => "uefi-loader-handoff.efi",
        LoaderMode::Chainload => "uefi-loader-chainload.efi",
    });
    fs::create_dir_all(
        staged_loader
            .parent()
            .expect("staged loader path always has a parent"),
    )?;
    fs::copy(&loader, &staged_loader).with_context(|| {
        format!(
            "copying {} to {}",
            loader.display(),
            staged_loader.display()
        )
    })?;

    Ok(staged_loader)
}

fn build_kernel_image(release: bool) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command.arg("build");
    command.arg("-p").arg("kernel-image");
    command.arg("--target").arg(KERNEL_TARGET);
    if release {
        command.arg("--release");
    }

    run(command, "building the standalone kernel image")?;

    let profile = if release { "release" } else { "debug" };
    let output_dir = workspace_root()
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile);
    let candidates = [
        output_dir.join(KERNEL_BIN),
        output_dir.join(format!("{KERNEL_BIN}.exe")),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!(
        "expected kernel image at {} or {}",
        output_dir.join(KERNEL_BIN).display(),
        output_dir.join(format!("{KERNEL_BIN}.exe")).display()
    );
}

fn make_image(loader: &Path, kernel_image: &Path, mode: LoaderMode) -> Result<PathBuf> {
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;

    let image_path = build_dir.join(match mode {
        LoaderMode::Interactive => "codexos-interactive.img",
        LoaderMode::Handoff => "codexos-handoff.img",
        LoaderMode::Chainload => "codexos-chainload.img",
    });
    let version = release_version()?;
    make_image_at(&image_path, loader, kernel_image, version)?;
    Ok(image_path)
}

fn make_gpt_image(loader: &Path, kernel_image: &Path, mode: LoaderMode) -> Result<PathBuf> {
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;

    let image_path = build_dir.join(match mode {
        LoaderMode::Interactive => "codexos-gpt-interactive.img",
        LoaderMode::Handoff => "codexos-gpt-handoff.img",
        LoaderMode::Chainload => "codexos-gpt-chainload.img",
    });
    let version = release_version()?;
    make_gpt_image_at(&image_path, loader, kernel_image, version)?;
    Ok(image_path)
}

fn make_image_at(
    image_path: &Path,
    loader: &Path,
    kernel_image: &Path,
    version: u64,
) -> Result<()> {
    let signing_identity = load_signing_identity()?;
    let next_trust_key = next_trust_key_from_env()?;
    make_image_at_with_identity(
        image_path,
        loader,
        kernel_image,
        version,
        &signing_identity,
        next_trust_key.as_ref(),
    )
}

fn make_gpt_image_at(
    image_path: &Path,
    loader: &Path,
    kernel_image: &Path,
    version: u64,
) -> Result<()> {
    let signing_identity = load_signing_identity()?;
    let next_trust_key = next_trust_key_from_env()?;
    make_gpt_image_at_with_identity(
        image_path,
        loader,
        kernel_image,
        version,
        &signing_identity,
        next_trust_key.as_ref(),
    )
}

fn make_image_at_with_identity(
    image_path: &Path,
    loader: &Path,
    kernel_image: &Path,
    version: u64,
    signing_identity: &SigningIdentity,
    next_trust_key: Option<&ed25519_dalek::VerifyingKey>,
) -> Result<()> {
    if version == 0 {
        bail!("release version must be greater than zero");
    }
    let image = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(image_path)
        .with_context(|| format!("creating {}", image_path.display()))?;
    image
        .set_len(IMAGE_SIZE_BYTES)
        .with_context(|| format!("resizing {}", image_path.display()))?;

    format_disk(image.try_clone()?)?;
    let kernel_bytes =
        fs::read(kernel_image).with_context(|| format!("reading {}", kernel_image.display()))?;
    let signed_manifest = sign_kernel_release_with_identity(
        &kernel_bytes,
        version,
        signing_identity,
        next_trust_key,
    )?;
    copy_artifacts_into_image(image, loader, &kernel_bytes, &signed_manifest)?;
    Ok(())
}

fn make_gpt_image_at_with_identity(
    image_path: &Path,
    loader: &Path,
    kernel_image: &Path,
    version: u64,
    signing_identity: &SigningIdentity,
    next_trust_key: Option<&ed25519_dalek::VerifyingKey>,
) -> Result<()> {
    if version == 0 {
        bail!("release version must be greater than zero");
    }
    let image = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(image_path)
        .with_context(|| format!("creating {}", image_path.display()))?;
    image
        .set_len(IMAGE_SIZE_BYTES)
        .with_context(|| format!("resizing {}", image_path.display()))?;
    write_gpt_disk_layout(&image)?;

    let esp = esp_layout()?;
    let mut volume = OffsetFile::new(
        image.try_clone()?,
        esp.first_lba * DISK_SECTOR_BYTES,
        esp.sector_count * DISK_SECTOR_BYTES,
    )?;
    format_fat_volume(&mut volume)?;
    let kernel_bytes =
        fs::read(kernel_image).with_context(|| format!("reading {}", kernel_image.display()))?;
    let signed_manifest = sign_kernel_release_with_identity(
        &kernel_bytes,
        version,
        signing_identity,
        next_trust_key,
    )?;
    copy_artifacts_into_image(volume, loader, &kernel_bytes, &signed_manifest)?;
    image
        .sync_all()
        .with_context(|| format!("flushing GPT image {}", image_path.display()))?;
    validate_gpt_esp_image(image_path)?;
    Ok(())
}

fn format_disk(mut image: File) -> Result<()> {
    image.seek(SeekFrom::Start(0))?;
    format_fat_volume(&mut image)
}

fn format_fat_volume<T: Read + Write + Seek>(volume: &mut T) -> Result<()> {
    volume.seek(SeekFrom::Start(0))?;
    fatfs::format_volume(volume, FormatVolumeOptions::new()).context("formatting FAT volume")?;
    Ok(())
}

fn copy_artifacts_into_image<T: Read + Write + Seek>(
    image: T,
    loader: &Path,
    kernel_bytes: &[u8],
    manifest: &[u8],
) -> Result<()> {
    let fs = FileSystem::new(image, FsOptions::new()).context("opening FAT filesystem")?;
    let root = fs.root_dir();

    create_dir_if_missing(&root, "EFI")?;
    let efi_dir = root.open_dir("EFI").context("opening EFI directory")?;
    create_dir_if_missing(&efi_dir, "BOOT")?;
    let boot_dir = efi_dir
        .open_dir("BOOT")
        .context("opening EFI/BOOT directory")?;

    let loader_bytes = fs::read(loader).with_context(|| format!("reading {}", loader.display()))?;
    let mut file = boot_dir
        .create_file("BOOTX64.EFI")
        .context("creating EFI/BOOT/BOOTX64.EFI")?;
    file.write_all(&loader_bytes)
        .context("writing UEFI loader into the image")?;
    file.flush().context("flushing FAT image")?;

    create_dir_if_missing(&root, "SYSTEM")?;
    let system = root
        .open_dir("SYSTEM")
        .context("opening SYSTEM directory")?;
    for slot in [SystemSlot::A, SystemSlot::B] {
        create_dir_if_missing(&system, slot.as_str())?;
        let directory = system
            .open_dir(slot.as_str())
            .with_context(|| format!("opening SYSTEM/{}", slot.as_str()))?;
        write_fat_file(&directory, "KERNEL.ELF", kernel_bytes)?;
        write_fat_file(&directory, "KERNEL.SIG", manifest)?;
    }
    let release = UpdateManifest::decode(manifest)
        .map_err(|error| anyhow::anyhow!("decoding generated manifest: {error:?}"))?;
    let first = BootState {
        generation: 1,
        active_slot: SystemSlot::A,
        active_release_version: release.release_version,
    };
    let second = BootState {
        generation: 2,
        ..first
    };
    write_fat_file(&root, "BOOTSTA0.BIN", &first.encode())?;
    write_fat_file(&root, "BOOTSTA1.BIN", &second.encode())?;
    Ok(())
}

#[derive(Clone, Copy)]
struct EspLayout {
    first_lba: u64,
    last_lba: u64,
    sector_count: u64,
}

struct OffsetFile {
    file: File,
    start: u64,
    length: u64,
    position: u64,
}

impl OffsetFile {
    fn new(file: File, start: u64, length: u64) -> Result<Self> {
        let end = start
            .checked_add(length)
            .context("offset-backed volume end overflow")?;
        let file_len = file
            .metadata()
            .context("reading offset-backed volume metadata")?
            .len();
        if end > file_len {
            bail!(
                "offset-backed volume exceeds image length: start={} length={} image={}",
                start,
                length,
                file_len
            );
        }
        Ok(Self {
            file,
            start,
            length,
            position: 0,
        })
    }
}

impl Read for OffsetFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.length {
            return Ok(0);
        }
        let remaining = usize::try_from((self.length - self.position).min(buffer.len() as u64))
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "volume read too large"))?;
        self.file
            .seek(SeekFrom::Start(self.start + self.position))?;
        let read = self.file.read(&mut buffer[..remaining])?;
        self.position = self.position.saturating_add(read as u64);
        Ok(read)
    }
}

impl Write for OffsetFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() as u64 > self.length.saturating_sub(self.position) {
            return Err(io::Error::new(
                ErrorKind::WriteZero,
                "write exceeds offset-backed volume",
            ));
        }
        self.file
            .seek(SeekFrom::Start(self.start + self.position))?;
        let written = self.file.write(buffer)?;
        self.position = self.position.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for OffsetFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if next < 0 || next > i128::from(self.length) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "seek outside offset-backed volume",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

fn esp_layout() -> Result<EspLayout> {
    if !IMAGE_SIZE_BYTES.is_multiple_of(DISK_SECTOR_BYTES) {
        bail!("disk image size must be sector aligned");
    }
    let total_lbas = IMAGE_SIZE_BYTES / DISK_SECTOR_BYTES;
    let entries_sectors = gpt_entry_array_sectors();
    let backup_entries_lba = total_lbas
        .checked_sub(1 + entries_sectors)
        .context("disk image is too small for backup GPT entries")?;
    let last_lba = backup_entries_lba
        .checked_sub(1)
        .context("disk image is too small for an ESP")?;
    if GPT_ESP_FIRST_LBA > last_lba {
        bail!("disk image is too small for the GPT ESP layout");
    }
    Ok(EspLayout {
        first_lba: GPT_ESP_FIRST_LBA,
        last_lba,
        sector_count: last_lba - GPT_ESP_FIRST_LBA + 1,
    })
}

fn gpt_entry_array_sectors() -> u64 {
    u64::from(GPT_ENTRY_COUNT * GPT_ENTRY_BYTES).div_ceil(DISK_SECTOR_BYTES)
}

fn write_gpt_disk_layout(image: &File) -> Result<()> {
    let mut disk = image.try_clone().context("cloning GPT image handle")?;
    let total_lbas = IMAGE_SIZE_BYTES / DISK_SECTOR_BYTES;
    let last_lba = total_lbas
        .checked_sub(1)
        .context("disk image has no sectors")?;
    let backup_entries_lba = last_lba - gpt_entry_array_sectors();
    let esp = esp_layout()?;

    write_protective_mbr(&mut disk, total_lbas)?;
    let entries = build_gpt_entries(esp)?;
    write_at_lba(&mut disk, GPT_PRIMARY_ENTRIES_LBA, &entries)?;
    write_at_lba(&mut disk, backup_entries_lba, &entries)?;
    let entries_crc = gpt_crc32(&entries);
    let primary = build_gpt_header(
        GPT_PRIMARY_HEADER_LBA,
        last_lba,
        GPT_PRIMARY_ENTRIES_LBA,
        entries_crc,
    )?;
    let backup = build_gpt_header(
        last_lba,
        GPT_PRIMARY_HEADER_LBA,
        backup_entries_lba,
        entries_crc,
    )?;
    write_at_lba(&mut disk, GPT_PRIMARY_HEADER_LBA, &primary)?;
    write_at_lba(&mut disk, last_lba, &backup)?;
    disk.sync_all().context("flushing GPT disk layout")?;
    Ok(())
}

fn write_protective_mbr(disk: &mut File, total_lbas: u64) -> Result<()> {
    let mut sector = [0_u8; DISK_SECTOR_BYTES as usize];
    sector[446] = 0x00;
    sector[450] = 0xee;
    sector[454..458].copy_from_slice(&1_u32.to_le_bytes());
    let protected_lbas = total_lbas.saturating_sub(1).min(u64::from(u32::MAX)) as u32;
    sector[458..462].copy_from_slice(&protected_lbas.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xaa;
    disk.seek(SeekFrom::Start(0))?;
    disk.write_all(&sector)?;
    Ok(())
}

fn build_gpt_entries(esp: EspLayout) -> Result<Vec<u8>> {
    let bytes = usize::try_from(GPT_ENTRY_COUNT * GPT_ENTRY_BYTES)
        .context("GPT entry array length overflow")?;
    let mut entries = vec![0_u8; bytes];
    entries[0..16].copy_from_slice(&EFI_SYSTEM_PARTITION_GUID);
    entries[16..32].copy_from_slice(&CODEXOS_ESP_GUID);
    put_le_u64(&mut entries, 32, esp.first_lba);
    put_le_u64(&mut entries, 40, esp.last_lba);
    write_utf16_partition_name(&mut entries[56..128], "codexOS EFI System")?;
    Ok(entries)
}

fn write_utf16_partition_name(output: &mut [u8], name: &str) -> Result<()> {
    let mut cursor = 0;
    for code_unit in name.encode_utf16() {
        if cursor + 2 > output.len() {
            bail!("GPT partition name is too long");
        }
        output[cursor..cursor + 2].copy_from_slice(&code_unit.to_le_bytes());
        cursor += 2;
    }
    Ok(())
}

fn build_gpt_header(
    current_lba: u64,
    alternate_lba: u64,
    entries_lba: u64,
    entries_crc: u32,
) -> Result<[u8; DISK_SECTOR_BYTES as usize]> {
    let mut header = [0_u8; DISK_SECTOR_BYTES as usize];
    let esp = esp_layout()?;
    header[0..8].copy_from_slice(b"EFI PART");
    put_le_u32(&mut header, 8, 0x0001_0000);
    put_le_u32(&mut header, 12, 92);
    put_le_u64(&mut header, 24, current_lba);
    put_le_u64(&mut header, 32, alternate_lba);
    put_le_u64(&mut header, 40, esp.first_lba);
    put_le_u64(&mut header, 48, esp.last_lba);
    header[56..72].copy_from_slice(&CODEXOS_DISK_GUID);
    put_le_u64(&mut header, 72, entries_lba);
    put_le_u32(&mut header, 80, GPT_ENTRY_COUNT);
    put_le_u32(&mut header, 84, GPT_ENTRY_BYTES);
    put_le_u32(&mut header, 88, entries_crc);
    let checksum = gpt_crc32(&header[..92]);
    put_le_u32(&mut header, 16, checksum);
    Ok(header)
}

fn write_at_lba(disk: &mut File, lba: u64, bytes: &[u8]) -> Result<()> {
    if !(bytes.len() as u64).is_multiple_of(DISK_SECTOR_BYTES) {
        bail!("GPT write length must be sector aligned");
    }
    disk.seek(SeekFrom::Start(lba * DISK_SECTOR_BYTES))?;
    disk.write_all(bytes)?;
    Ok(())
}

fn validate_gpt_esp_image(image_path: &Path) -> Result<()> {
    let mut disk = OpenOptions::new()
        .read(true)
        .open(image_path)
        .with_context(|| format!("opening GPT image {}", image_path.display()))?;
    let esp = validate_gpt_esp_layout(&mut disk)?;

    let volume = OffsetFile::new(
        disk.try_clone()?,
        esp.first_lba * DISK_SECTOR_BYTES,
        esp.sector_count * DISK_SECTOR_BYTES,
    )?;
    let filesystem =
        FileSystem::new(volume, FsOptions::new()).context("opening GPT EFI System Partition")?;
    let root = filesystem.root_dir();
    let efi_loader = read_fat_file(&root, "EFI/BOOT/BOOTX64.EFI")?;
    if efi_loader.is_empty() {
        bail!("GPT EFI System Partition has an empty BOOTX64.EFI");
    }
    let boot_state = latest_boot_state(&root)?;
    if boot_state.active_release_version == 0 {
        bail!("GPT EFI System Partition boot state is invalid");
    }
    let signing_identity = load_signing_identity()?;
    validate_signed_system_slot(
        &root,
        boot_state.active_slot,
        boot_state.active_release_version,
        &signing_identity.verifying_key,
    )?;
    validate_signed_system_slot(
        &root,
        boot_state.active_slot.other(),
        boot_state.active_release_version,
        &signing_identity.verifying_key,
    )?;
    Ok(())
}

fn validate_gpt_esp_layout(disk: &mut File) -> Result<EspLayout> {
    let esp = esp_layout()?;
    let image_len = disk.metadata().context("reading GPT image metadata")?.len();
    if image_len < IMAGE_SIZE_BYTES || !image_len.is_multiple_of(DISK_SECTOR_BYTES) {
        bail!("GPT image size is invalid: {image_len} bytes");
    }
    validate_protective_mbr(disk)?;
    let entries = read_at_lba(disk, GPT_PRIMARY_ENTRIES_LBA, gpt_entry_array_sectors())?;
    let entries_crc = gpt_crc32(&entries);
    validate_gpt_entries(&entries, esp)?;

    let last_lba = image_len / DISK_SECTOR_BYTES - 1;
    let backup_entries_lba = last_lba - gpt_entry_array_sectors();
    validate_gpt_header(
        &read_at_lba(disk, GPT_PRIMARY_HEADER_LBA, 1)?,
        GPT_PRIMARY_HEADER_LBA,
        last_lba,
        GPT_PRIMARY_ENTRIES_LBA,
        entries_crc,
    )?;
    validate_gpt_header(
        &read_at_lba(disk, last_lba, 1)?,
        last_lba,
        GPT_PRIMARY_HEADER_LBA,
        backup_entries_lba,
        entries_crc,
    )?;
    let backup_entries = read_at_lba(disk, backup_entries_lba, gpt_entry_array_sectors())?;
    if backup_entries != entries {
        bail!("backup GPT entries differ from primary entries");
    }
    Ok(esp)
}

fn validate_signed_system_slot<T: Read + Write + Seek>(
    root: &fatfs::Dir<'_, T>,
    slot: SystemSlot,
    expected_version: u64,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<()> {
    let kernel_name = format!("SYSTEM/{}/KERNEL.ELF", slot.as_str());
    let manifest_name = format!("SYSTEM/{}/KERNEL.SIG", slot.as_str());
    let kernel = read_fat_file(root, &kernel_name)?;
    let manifest = read_fat_file(root, &manifest_name)?;
    let release = verify_signed_release_with_key(&kernel, &manifest, verifying_key)
        .with_context(|| format!("verifying signed system slot {}", slot.as_str()))?;
    if release.release_version != expected_version {
        bail!(
            "signed system slot {} has release version {}, expected {}",
            slot.as_str(),
            release.release_version,
            expected_version
        );
    }
    Ok(())
}

fn validate_protective_mbr(disk: &mut File) -> Result<()> {
    let sector = read_at_lba(disk, 0, 1)?;
    if sector[510] != 0x55 || sector[511] != 0xaa || sector[450] != 0xee {
        bail!("protective MBR is missing or invalid");
    }
    Ok(())
}

fn validate_gpt_entries(entries: &[u8], esp: EspLayout) -> Result<()> {
    if entries.get(0..16) != Some(EFI_SYSTEM_PARTITION_GUID.as_slice())
        || entries.get(16..32) != Some(CODEXOS_ESP_GUID.as_slice())
        || get_le_u64(entries, 32)? != esp.first_lba
        || get_le_u64(entries, 40)? != esp.last_lba
    {
        bail!("GPT EFI System Partition entry is invalid");
    }
    Ok(())
}

fn validate_gpt_header(
    header: &[u8],
    current_lba: u64,
    alternate_lba: u64,
    entries_lba: u64,
    entries_crc: u32,
) -> Result<()> {
    if header.len() != DISK_SECTOR_BYTES as usize
        || header.get(0..8) != Some(b"EFI PART".as_slice())
        || get_le_u32(header, 8)? != 0x0001_0000
        || get_le_u32(header, 12)? != 92
        || get_le_u64(header, 24)? != current_lba
        || get_le_u64(header, 32)? != alternate_lba
        || get_le_u64(header, 72)? != entries_lba
        || get_le_u32(header, 80)? != GPT_ENTRY_COUNT
        || get_le_u32(header, 84)? != GPT_ENTRY_BYTES
        || get_le_u32(header, 88)? != entries_crc
    {
        bail!("GPT header fields are invalid");
    }
    let stored_crc = get_le_u32(header, 16)?;
    let mut copy = header[..92].to_vec();
    put_le_u32(&mut copy, 16, 0);
    if gpt_crc32(&copy) != stored_crc {
        bail!("GPT header CRC is invalid");
    }
    Ok(())
}

fn read_at_lba(disk: &mut File, lba: u64, sectors: u64) -> Result<Vec<u8>> {
    let bytes = sectors
        .checked_mul(DISK_SECTOR_BYTES)
        .and_then(|value| usize::try_from(value).ok())
        .context("sector read length overflow")?;
    let mut output = vec![0_u8; bytes];
    disk.seek(SeekFrom::Start(lba * DISK_SECTOR_BYTES))?;
    disk.read_exact(&mut output)?;
    Ok(output)
}

fn put_le_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_le_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_le_u32(input: &[u8], offset: usize) -> Result<u32> {
    let bytes = input
        .get(offset..offset + 4)
        .context("reading little-endian u32 outside buffer")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn get_le_u64(input: &[u8], offset: usize) -> Result<u64> {
    let bytes = input
        .get(offset..offset + 8)
        .context("reading little-endian u64 outside buffer")?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn gpt_crc32(bytes: &[u8]) -> u32 {
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

fn write_fat_file<T: Read + Write + Seek>(
    dir: &fatfs::Dir<'_, T>,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut file = dir
        .create_file(name)
        .with_context(|| format!("creating {name}"))?;
    file.truncate()
        .with_context(|| format!("truncating {name}"))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {name}"))?;
    file.flush().with_context(|| format!("flushing {name}"))?;
    Ok(())
}

fn install_system(destination: &Path) -> Result<()> {
    if destination.exists() {
        bail!(
            "installation destination already exists: {}; refusing to overwrite it",
            destination.display()
        );
    }
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating installation directory {}", parent.display()))?;
    }
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let source = make_image(&loader, &kernel, LoaderMode::Chainload)?;
    let expected_hash = sha256_file(&source)?;

    let mut input = File::open(&source)
        .with_context(|| format!("opening installer source {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating installation target {}", destination.display()))?;
    io::copy(&mut input, &mut output).with_context(|| {
        format!(
            "copying installation image {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    output
        .sync_all()
        .with_context(|| format!("flushing installation target {}", destination.display()))?;
    drop(output);
    let installed_hash = sha256_file(destination)?;
    if installed_hash != expected_hash {
        let _ = fs::remove_file(destination);
        bail!(
            "installation readback hash mismatch for {}; incomplete target was removed",
            destination.display()
        );
    }
    println!(
        "installation verified: target={} bytes={} sha256={}",
        destination.display(),
        fs::metadata(destination)?.len(),
        hex_encode(&installed_hash)
    );
    Ok(())
}

fn install_gpt_system(destination: &Path) -> Result<()> {
    if destination.exists() {
        bail!(
            "installation destination already exists: {}; refusing to overwrite it",
            destination.display()
        );
    }
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating installation directory {}", parent.display()))?;
    }
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let source = make_gpt_image(&loader, &kernel, LoaderMode::Chainload)?;
    let expected_hash = sha256_file(&source)?;

    let mut input = File::open(&source)
        .with_context(|| format!("opening GPT installer source {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating GPT installation target {}", destination.display()))?;
    io::copy(&mut input, &mut output).with_context(|| {
        format!(
            "copying GPT installation image {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    output
        .sync_all()
        .with_context(|| format!("flushing GPT installation target {}", destination.display()))?;
    drop(output);
    let installed_hash = sha256_file(destination)?;
    if installed_hash != expected_hash {
        let _ = fs::remove_file(destination);
        bail!(
            "GPT installation readback hash mismatch for {}; incomplete target was removed",
            destination.display()
        );
    }
    validate_gpt_esp_image(destination)?;
    println!(
        "gpt installation verified: target={} bytes={} sha256={}",
        destination.display(),
        fs::metadata(destination)?.len(),
        hex_encode(&installed_hash)
    );
    Ok(())
}

fn build_production_release(destination: &Path) -> Result<()> {
    let key = env::var_os("CODEXOS_SIGNING_KEY")
        .context("production release requires CODEXOS_SIGNING_KEY")?;
    let key_path = PathBuf::from(key);
    if !key_path.is_file() {
        bail!(
            "CODEXOS_SIGNING_KEY does not point to a 32-byte key file: {}",
            key_path.display()
        );
    }
    env::var("CODEXOS_RELEASE_VERSION")
        .context("production release requires an explicit CODEXOS_RELEASE_VERSION")?;
    let version = release_version()?;
    println!(
        "production release authorization: version={} external-key={}",
        version,
        key_path.display()
    );
    install_system(destination)
}

fn build_production_gpt_release(destination: &Path) -> Result<()> {
    let key = env::var_os("CODEXOS_SIGNING_KEY")
        .context("production GPT release requires CODEXOS_SIGNING_KEY")?;
    let key_path = PathBuf::from(key);
    if !key_path.is_file() {
        bail!(
            "CODEXOS_SIGNING_KEY does not point to a 32-byte key file: {}",
            key_path.display()
        );
    }
    env::var("CODEXOS_RELEASE_VERSION")
        .context("production GPT release requires an explicit CODEXOS_RELEASE_VERSION")?;
    let version = release_version()?;
    println!(
        "production GPT release authorization: version={} external-key={}",
        version,
        key_path.display()
    );
    install_gpt_system(destination)
}

fn derive_public_key(seed_path: &Path, output_path: &Path) -> Result<()> {
    if output_path.exists() {
        bail!(
            "public key destination already exists: {}; refusing to overwrite it",
            output_path.display()
        );
    }
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating public key directory {}", parent.display()))?;
    }
    let bytes = fs::read(seed_path)
        .with_context(|| format!("reading signing seed {}", seed_path.display()))?;
    let mut seed: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!(
            "signing seed {} is {} bytes; expected exactly 32 raw bytes",
            seed_path.display(),
            bytes.len()
        )
    })?;
    let signing_key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    let public_key = signing_key.verifying_key();
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
        .with_context(|| format!("creating public key {}", output_path.display()))?;
    output
        .write_all(public_key.as_bytes())
        .with_context(|| format!("writing public key {}", output_path.display()))?;
    output
        .sync_all()
        .with_context(|| format!("flushing public key {}", output_path.display()))?;
    println!(
        "public key exported: path={} key-id={}",
        output_path.display(),
        hex_encode(&key_id_for_public_key(public_key.as_bytes()))
    );
    Ok(())
}

fn apply_signed_update(image: &Path) -> Result<()> {
    apply_signed_update_version(image, release_version()?)
}

fn apply_signed_update_version(image: &Path, version: u64) -> Result<()> {
    if !image.is_file() {
        bail!("installed image does not exist: {}", image.display());
    }
    let kernel_path = build_kernel_image(true)?;
    let kernel =
        fs::read(&kernel_path).with_context(|| format!("reading {}", kernel_path.display()))?;
    if version == 0 {
        bail!("release version must be greater than zero");
    }
    let signing_identity = load_signing_identity()?;
    let next_trust_key = next_trust_key_from_env()?;
    let manifest = sign_kernel_release_with_identity(
        &kernel,
        version,
        &signing_identity,
        next_trust_key.as_ref(),
    )?;
    verify_signed_release_with_key(&kernel, &manifest, &signing_identity.verifying_key)?;

    let disk = OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .with_context(|| format!("opening installed image {}", image.display()))?;
    let layout = detect_installed_layout(&disk)?;
    let next_state = match layout {
        InstalledLayout::WholeDiskFat => {
            let mut volume = disk
                .try_clone()
                .with_context(|| format!("cloning installed image handle {}", image.display()))?;
            volume
                .seek(SeekFrom::Start(0))
                .with_context(|| format!("rewinding installed image {}", image.display()))?;
            let filesystem = FileSystem::new(volume, FsOptions::new())
                .with_context(|| format!("opening installed FAT filesystem {}", image.display()))?;
            apply_signed_update_to_filesystem(
                filesystem,
                image,
                &kernel,
                &manifest,
                &signing_identity.verifying_key,
                version,
            )?
        }
        InstalledLayout::GptEsp(esp) => {
            let volume = OffsetFile::new(
                disk.try_clone()?,
                esp.first_lba * DISK_SECTOR_BYTES,
                esp.sector_count * DISK_SECTOR_BYTES,
            )?;
            let filesystem = FileSystem::new(volume, FsOptions::new()).with_context(|| {
                format!(
                    "opening installed GPT EFI System Partition {}",
                    image.display()
                )
            })?;
            let next_state = apply_signed_update_to_filesystem(
                filesystem,
                image,
                &kernel,
                &manifest,
                &signing_identity.verifying_key,
                version,
            )?;
            validate_gpt_esp_image(image)?;
            next_state
        }
    };
    disk.sync_all()
        .with_context(|| format!("flushing installed image {}", image.display()))?;
    println!(
        "signed update committed: image={} layout={} slot={} version={} generation={} redundant-copy=verified",
        image.display(),
        layout.as_str(),
        next_state.active_slot.as_str(),
        next_state.active_release_version,
        next_state.generation
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum InstalledLayout {
    WholeDiskFat,
    GptEsp(EspLayout),
}

impl InstalledLayout {
    const fn as_str(self) -> &'static str {
        match self {
            Self::WholeDiskFat => "whole-disk-fat",
            Self::GptEsp(_) => "gpt-esp",
        }
    }
}

fn detect_installed_layout(disk: &File) -> Result<InstalledLayout> {
    let mut clone = disk
        .try_clone()
        .context("cloning installed image for layout detection")?;
    if let Ok(esp) = validate_gpt_esp_layout(&mut clone) {
        return Ok(InstalledLayout::GptEsp(esp));
    }
    Ok(InstalledLayout::WholeDiskFat)
}

fn apply_signed_update_to_filesystem<T: Read + Write + Seek>(
    filesystem: FileSystem<T>,
    image: &Path,
    kernel: &[u8],
    manifest: &[u8],
    verifying_key: &ed25519_dalek::VerifyingKey,
    version: u64,
) -> Result<BootState> {
    let next_state;
    {
        let root = filesystem.root_dir();
        let current = latest_boot_state(&root)?;
        if version <= current.active_release_version {
            bail!(
                "update version {} must be greater than installed active version {}",
                version,
                current.active_release_version
            );
        }
        let inactive = current.active_slot.other();
        let system = root
            .open_dir("SYSTEM")
            .context("opening SYSTEM directory")?;
        let slot = system
            .open_dir(inactive.as_str())
            .with_context(|| format!("opening inactive slot {}", inactive.as_str()))?;
        write_fat_file(&slot, "KERNEL.ELF", kernel)?;
        write_fat_file(&slot, "KERNEL.SIG", manifest)?;

        let kernel_readback = read_fat_file(&slot, "KERNEL.ELF")?;
        let manifest_readback = read_fat_file(&slot, "KERNEL.SIG")?;
        verify_signed_release_with_key(&kernel_readback, &manifest_readback, verifying_key)?;
        if kernel_readback != kernel || manifest_readback != manifest {
            bail!("inactive slot readback differs from the signed update payload");
        }

        let generation = current
            .generation
            .checked_add(1)
            .context("boot-state generation exhausted")?;
        next_state = BootState {
            generation,
            active_slot: inactive,
            active_release_version: version,
        };
        let state_name = if generation & 1 == 1 {
            "BOOTSTA0.BIN"
        } else {
            "BOOTSTA1.BIN"
        };
        write_fat_file(&root, state_name, &next_state.encode())?;
        if latest_boot_state(&root)? != next_state {
            bail!("boot-state readback did not select the newly written generation");
        }

        let redundant = system
            .open_dir(current.active_slot.as_str())
            .with_context(|| {
                format!(
                    "opening previous slot {} for redundancy synchronization",
                    current.active_slot.as_str()
                )
            })?;
        write_fat_file(&redundant, "KERNEL.ELF", kernel)?;
        write_fat_file(&redundant, "KERNEL.SIG", manifest)?;
        let redundant_kernel = read_fat_file(&redundant, "KERNEL.ELF")?;
        let redundant_manifest = read_fat_file(&redundant, "KERNEL.SIG")?;
        verify_signed_release_with_key(&redundant_kernel, &redundant_manifest, verifying_key)?;
        if redundant_kernel != kernel || redundant_manifest != manifest {
            bail!("redundant slot readback differs from the signed update payload");
        }
    }
    filesystem
        .unmount()
        .with_context(|| format!("unmounting installed filesystem {}", image.display()))?;
    Ok(next_state)
}

fn latest_boot_state<T: Read + Write + Seek>(root: &fatfs::Dir<'_, T>) -> Result<BootState> {
    let mut selected = None;
    for name in ["BOOTSTA0.BIN", "BOOTSTA1.BIN"] {
        let Ok(bytes) = read_fat_file(root, name) else {
            continue;
        };
        let Ok(state) = BootState::decode(&bytes) else {
            continue;
        };
        if selected
            .as_ref()
            .is_none_or(|current: &BootState| state.generation > current.generation)
        {
            selected = Some(state);
        }
    }
    selected.context("installed image has no valid boot-state record")
}

fn read_fat_file<T: Read + Write + Seek>(dir: &fatfs::Dir<'_, T>, name: &str) -> Result<Vec<u8>> {
    let mut file = dir
        .open_file(name)
        .with_context(|| format!("opening {name}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("reading {name}"))?;
    Ok(bytes)
}

fn verify_signed_release_with_key(
    kernel: &[u8],
    encoded_manifest: &[u8],
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<UpdateManifest> {
    let manifest = UpdateManifest::decode(encoded_manifest)
        .map_err(|error| anyhow::anyhow!("decoding signed release manifest: {error:?}"))?;
    if u64::try_from(kernel.len()).ok() != Some(manifest.kernel_size) {
        bail!("signed release kernel size does not match its manifest");
    }
    let hash: [u8; 32] = Sha256::digest(kernel).into();
    if hash != manifest.kernel_sha256 {
        bail!("signed release kernel hash does not match its manifest");
    }
    let key_id = key_id_for_public_key(verifying_key.as_bytes());
    if key_id != manifest.key_id {
        bail!("signed release key identifier does not match the configured signing identity");
    }
    validate_manifest_next_trust_key(&manifest)?;
    verifying_key
        .verify_strict(
            &manifest.signing_bytes()[..manifest.signing_len()],
            &Signature::from_bytes(&manifest.signature),
        )
        .map_err(|error| anyhow::anyhow!("verifying Ed25519 release signature: {error}"))?;
    Ok(manifest)
}

fn validate_manifest_next_trust_key(manifest: &UpdateManifest) -> Result<()> {
    if !manifest.has_next_trust_key() {
        return Ok(());
    }
    ed25519_dalek::VerifyingKey::from_bytes(&manifest.next_trust_public_key)
        .map_err(|error| anyhow::anyhow!("validating next trust root public key: {error}"))?;
    let next_key_id = key_id_for_public_key(&manifest.next_trust_public_key);
    if next_key_id != manifest.next_trust_key_id {
        bail!("signed release next trust root key identifier does not match its public key");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

struct SigningIdentity {
    signing_key: SigningKey,
    verifying_key: ed25519_dalek::VerifyingKey,
}

fn generate_signing_identity() -> Result<SigningIdentity> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|error| {
        anyhow::anyhow!("obtaining operating-system randomness for signing key: {error}")
    })?;
    let signing_key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    let verifying_key = signing_key.verifying_key();
    Ok(SigningIdentity {
        signing_key,
        verifying_key,
    })
}

fn load_signing_identity() -> Result<SigningIdentity> {
    let explicit = env::var_os("CODEXOS_SIGNING_KEY").map(PathBuf::from);
    let key_path = explicit
        .clone()
        .unwrap_or_else(|| workspace_root().join("build").join(DEVELOPMENT_SIGNING_KEY));
    if explicit.is_none() && !key_path.exists() {
        let parent = key_path
            .parent()
            .context("development signing key path has no parent")?;
        fs::create_dir_all(parent).context("creating signing key directory")?;
        let mut seed = [0_u8; 32];
        getrandom::fill(&mut seed).map_err(|error| {
            anyhow::anyhow!("obtaining operating-system randomness for signing key: {error}")
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&key_path)
            .with_context(|| format!("creating signing key {}", key_path.display()))?;
        file.write_all(&seed)
            .with_context(|| format!("writing signing key {}", key_path.display()))?;
        file.sync_all()
            .with_context(|| format!("flushing signing key {}", key_path.display()))?;
        seed.fill(0);
    }
    let bytes = fs::read(&key_path)
        .with_context(|| format!("reading signing key {}", key_path.display()))?;
    let mut seed: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!(
            "signing key {} is {} bytes; expected exactly 32 raw bytes",
            key_path.display(),
            bytes.len()
        )
    })?;
    let signing_key = SigningKey::from_bytes(&seed);
    seed.fill(0);
    let verifying_key = signing_key.verifying_key();
    Ok(SigningIdentity {
        signing_key,
        verifying_key,
    })
}

fn next_trust_key_from_env() -> Result<Option<ed25519_dalek::VerifyingKey>> {
    if let Ok(raw) = env::var("CODEXOS_NEXT_TRUST_PUBLIC_KEY_HEX") {
        let bytes =
            decode_hex_public_key(&raw).context("parsing CODEXOS_NEXT_TRUST_PUBLIC_KEY_HEX")?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes).map_err(|error| {
            anyhow::anyhow!("validating CODEXOS_NEXT_TRUST_PUBLIC_KEY_HEX: {error}")
        })?;
        return Ok(Some(key));
    }
    let Some(path) = env::var_os("CODEXOS_NEXT_TRUST_PUBLIC_KEY").map(PathBuf::from) else {
        return Ok(None);
    };
    let bytes = fs::read(&path)
        .with_context(|| format!("reading next trust root public key {}", path.display()))?;
    let bytes: [u8; PUBLIC_KEY_BYTES] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!(
            "next trust root public key {} is {} bytes; expected exactly 32 raw bytes",
            path.display(),
            bytes.len()
        )
    })?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
        .map_err(|error| anyhow::anyhow!("validating next trust root public key: {error}"))?;
    Ok(Some(key))
}

fn decode_hex_public_key(raw: &str) -> Result<[u8; PUBLIC_KEY_BYTES]> {
    if raw.len() != PUBLIC_KEY_BYTES * 2 {
        bail!(
            "hex public key has {} characters; expected {}",
            raw.len(),
            PUBLIC_KEY_BYTES * 2
        );
    }
    let mut bytes = [0_u8; PUBLIC_KEY_BYTES];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let high = hex_nibble(raw.as_bytes()[index * 2])
            .with_context(|| format!("hex public key byte {index} has an invalid high nibble"))?;
        let low = hex_nibble(raw.as_bytes()[index * 2 + 1])
            .with_context(|| format!("hex public key byte {index} has an invalid low nibble"))?;
        *byte = high << 4 | low;
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn key_id_for_public_key(public_key: &[u8; PUBLIC_KEY_BYTES]) -> [u8; KEY_ID_BYTES] {
    let public_hash = Sha256::digest(public_key);
    let mut key_id = [0_u8; KEY_ID_BYTES];
    key_id.copy_from_slice(&public_hash[..KEY_ID_BYTES]);
    key_id
}

fn release_version() -> Result<u64> {
    let raw = env::var("CODEXOS_RELEASE_VERSION").unwrap_or_else(|_| String::from("1"));
    let version = raw
        .parse::<u64>()
        .with_context(|| format!("parsing CODEXOS_RELEASE_VERSION={raw}"))?;
    if version == 0 {
        bail!("CODEXOS_RELEASE_VERSION must be greater than zero");
    }
    Ok(version)
}

fn sign_kernel_release_with_identity(
    kernel_bytes: &[u8],
    release_version: u64,
    identity: &SigningIdentity,
    next_trust_key: Option<&ed25519_dalek::VerifyingKey>,
) -> Result<[u8; codex_release::MANIFEST_BYTES]> {
    let kernel_size = u64::try_from(kernel_bytes.len()).context("kernel image is too large")?;
    let kernel_sha256: [u8; 32] = Sha256::digest(kernel_bytes).into();
    let key_id = key_id_for_public_key(identity.verifying_key.as_bytes());
    let mut manifest = if let Some(next_trust_key) = next_trust_key {
        let next_trust_key_id = key_id_for_public_key(next_trust_key.as_bytes());
        UpdateManifest::unsigned_with_next_trust_key(
            release_version,
            kernel_size,
            kernel_sha256,
            key_id,
            *next_trust_key.as_bytes(),
            next_trust_key_id,
        )
    } else {
        UpdateManifest::unsigned(release_version, kernel_size, kernel_sha256, key_id)
    };
    manifest.signature = identity
        .signing_key
        .sign(&manifest.signing_bytes()[..manifest.signing_len()])
        .to_bytes();
    if manifest.has_next_trust_key() {
        println!(
            "signed release: version={} kernel={} bytes sha256={} key-id={} next-trust-key-id={}",
            release_version,
            kernel_size,
            hex_encode(&kernel_sha256),
            hex_encode(&key_id),
            hex_encode(&manifest.next_trust_key_id)
        );
    } else {
        println!(
            "signed release: version={} kernel={} bytes sha256={} key-id={}",
            release_version,
            kernel_size,
            hex_encode(&kernel_sha256),
            hex_encode(&key_id)
        );
    }
    Ok(manifest.encode())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn create_dir_if_missing<T: Read + Write + Seek>(
    dir: &fatfs::Dir<'_, T>,
    name: &str,
) -> Result<()> {
    match dir.create_dir(name) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err).with_context(|| format!("creating directory {name}")),
    }
}

fn run_qemu(image: &Path, debug_wait: bool) -> Result<()> {
    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = persistent_ovmf_vars()?;
    let data_image = persistent_data_image()?;

    let mut command = Command::new(qemu);
    command.arg("-machine").arg("q35");
    command.arg("-m").arg("512");
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    ));
    command
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", ovmf_vars.display()));
    command
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
    attach_persistent_data_disk(&mut command, &data_image);
    attach_user_network(&mut command, None);
    command.arg("-serial").arg("stdio");
    command.arg("-monitor").arg("none");

    if debug_wait {
        command.arg("-s");
        command.arg("-S");
    }

    run(command, "running QEMU")
}

fn smoke_qemu(image: &Path, mode: LoaderMode) -> Result<()> {
    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = persistent_ovmf_vars()?;
    let data_image = persistent_data_image()?;
    let serial_log = workspace_root().join("build").join(match mode {
        LoaderMode::Interactive => "serial-interactive.log",
        LoaderMode::Handoff => "serial-handoff.log",
        LoaderMode::Chainload => "serial-chainload.log",
    });
    let log = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, image, &data_image, &serial_log)?;
    let reached_target = match mode {
        LoaderMode::Interactive | LoaderMode::Handoff => log.contains("codexOS kernel entered"),
        LoaderMode::Chainload => log.contains("codexOS standalone kernel entered"),
    };
    if !reached_target {
        bail!("smoke boot did not reach the expected target; serial output:\n{log}");
    }
    if !log.contains("kernel image:") {
        bail!("smoke boot did not inspect the standalone kernel image; serial output:\n{log}");
    }
    if !log.contains("kernel signature: verified=true version=") {
        bail!("smoke boot did not verify the signed kernel release; serial output:\n{log}");
    }
    if log.contains("[PANIC]") || log.contains("panicked at") {
        bail!("smoke boot reached the kernel but then panicked; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Handoff) && !log.contains("boot mode: post-exit-boot-services") {
        bail!("handoff smoke boot did not reach post-EBS mode; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Handoff) && !log.contains("vm switched to kernel page tables") {
        bail!("handoff smoke boot did not activate the kernel page table; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Handoff) && !log.contains("vm hhdm probe:") {
        bail!(
            "handoff smoke boot did not verify the higher-half direct map; serial output:\n{log}"
        );
    }
    if matches!(mode, LoaderMode::Handoff) && !log.contains("vm framebuffer hhdm:") {
        bail!(
            "handoff smoke boot did not remap the framebuffer through the higher-half direct map; serial output:\n{log}"
        );
    }
    if matches!(mode, LoaderMode::Handoff) && !log.contains("vm reserved hhdm:") {
        bail!(
            "handoff smoke boot did not report higher-half aliases for reserved handoff objects; serial output:\n{log}"
        );
    }
    if !matches!(mode, LoaderMode::Interactive) && !log.contains("vm kernel permissions:") {
        bail!("post-EBS smoke boot did not report kernel page permissions; serial output:\n{log}");
    }
    if !matches!(mode, LoaderMode::Interactive) && !log.contains(" wx=0") {
        bail!("post-EBS smoke boot found writable executable kernel pages; serial output:\n{log}");
    }
    if !matches!(mode, LoaderMode::Interactive)
        && !log.contains("interrupts: exception path verified")
    {
        bail!("post-EBS smoke boot did not verify its exception path; serial output:\n{log}");
    }
    if !matches!(mode, LoaderMode::Interactive)
        && !log.contains("interrupts: gdt=loaded tr=0x0018 idt=loaded")
    {
        bail!("post-EBS smoke boot did not load its kernel GDT and TSS; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("standalone boot info present") {
        bail!("chainload smoke boot did not enter the standalone kernel; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("standalone desktop rendered") {
        bail!("chainload smoke boot did not render the standalone desktop; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload)
        && !log.contains("process[1]: user log: ring3 syscall boundary active")
    {
        bail!("chainload smoke boot did not execute a Ring 3 system call; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload)
        && !log.contains("process[1]: kernel write denied addr=0x0000000000200000 err=0x7")
    {
        bail!("chainload smoke boot did not deny the Ring 3 kernel write; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("process isolation verified: pid=1") {
        bail!(
            "chainload smoke boot did not contain the user fault and resume the kernel; serial output:\n{log}"
        );
    }
    if matches!(mode, LoaderMode::Chainload)
        && !log.contains("standalone vm adopted current page tables")
    {
        bail!("chainload smoke boot did not adopt the loader page tables; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("standalone heap: capacity=") {
        bail!("chainload smoke boot did not initialize its reserved heap; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("standalone exception path verified")
    {
        bail!(
            "chainload smoke boot did not verify the resident exception path; serial output:\n{log}"
        );
    }
    if matches!(mode, LoaderMode::Chainload)
        && !log.contains("standalone descriptor tables: gdt=true tr=0x0018 idt=true")
    {
        bail!(
            "chainload smoke boot did not load resident descriptor tables; serial output:\n{log}"
        );
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("standalone timer interrupts active")
    {
        bail!("chainload smoke boot did not activate timer interrupts; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("scheduler verified: processes=3") {
        bail!(
            "chainload smoke boot did not verify preemptive process scheduling; serial output:\n{log}"
        );
    }
    if matches!(mode, LoaderMode::Chainload) {
        validate_persistent_executable_log(&log)?;
    }
    if matches!(mode, LoaderMode::Chainload) && !has_applied_kernel_relocations(&log) {
        bail!("chainload smoke boot did not apply kernel ELF relocations; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) {
        validate_hardware_log(&log)?;
        validate_filesystem_log(&log)?;
        validate_network_log(&log)?;
        validate_pointer_driver_log(&log)?;
    }

    let persistence_proof = if matches!(mode, LoaderMode::Chainload) {
        if !log.contains("virtio-blk: pci=") || !log.contains("codexfs: mounted state=") {
            bail!(
                "chainload smoke boot did not mount the persistent block filesystem; serial output:\n{log}"
            );
        }
        let first = persistence_counter(&log)?;
        let second_log =
            launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, image, &data_image, &serial_log)?;
        validate_hardware_log(&second_log)?;
        validate_filesystem_log(&second_log)?;
        validate_network_log(&second_log)?;
        validate_pointer_driver_log(&second_log)?;
        validate_persistent_executable_log(&second_log)?;
        if second_log.contains("[PANIC]")
            || second_log.contains("panicked at")
            || !second_log.contains("codexfs: mounted state=existing")
        {
            bail!(
                "persistence reboot did not remount a healthy existing filesystem; serial output:\n{second_log}"
            );
        }
        let second = persistence_counter(&second_log)?;
        let expected_first = first
            .0
            .checked_add(1)
            .context("boot counter overflow in first persistence boot")?;
        let expected_second = second
            .0
            .checked_add(1)
            .context("boot counter overflow in second persistence boot")?;
        if first.1 != expected_first || second.0 != first.1 || second.1 != expected_second {
            bail!(
                "persistent boot counter was not continuous across restarts: first={}->{}, second={}->{}",
                first.0,
                first.1,
                second.0,
                second.1
            );
        }
        Some((first, second, second_log))
    } else {
        None
    };

    println!("smoke check passed");
    println!("{log}");
    if let Some((first, second, second_log)) = persistence_proof {
        println!(
            "persistence reboot proof passed: {} -> {} -> {}",
            first.0, first.1, second.1
        );
        println!("{second_log}");
    }
    Ok(())
}

fn smoke_security() -> Result<()> {
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let version_one = build_dir.join("security-release-v1.img");
    let version_two = build_dir.join("security-release-v2.img");
    make_image_at(&version_one, &loader, &kernel, 1)?;
    make_image_at(&version_two, &loader, &kernel, 2)?;

    let tampered_kernel = build_dir.join("security-tampered-kernel.img");
    fs::copy(&version_two, &tampered_kernel).context("copying kernel tamper test image")?;
    mutate_fat_file_byte(&tampered_kernel, "SYSTEM/A/KERNEL.ELF", 0)?;
    mutate_fat_file_byte(&tampered_kernel, "SYSTEM/B/KERNEL.ELF", 0)?;
    let tampered_signature = build_dir.join("security-tampered-signature.img");
    fs::copy(&version_two, &tampered_signature).context("copying signature tamper test image")?;
    mutate_fat_file_byte(&tampered_signature, "SYSTEM/A/KERNEL.SIG", 160)?;
    mutate_fat_file_byte(&tampered_signature, "SYSTEM/B/KERNEL.SIG", 160)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_security_ovmf_vars()?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-security.log");

    let valid = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &version_two,
        &data_image,
        &serial_log,
    )?;
    if !valid.contains("kernel signature: verified=true version=2")
        || !valid.contains("codexOS standalone kernel entered")
    {
        bail!("security smoke could not establish release version 2; serial output:\n{valid}");
    }

    let rollback = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &version_one,
        &data_image,
        &serial_log,
    )?;
    require_security_rejection(&rollback, "RollbackDetected { candidate: 1, minimum: 2 }")?;

    let changed_kernel = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &tampered_kernel,
        &data_image,
        &serial_log,
    )?;
    require_security_rejection(&changed_kernel, "KernelHashMismatch")?;

    let changed_signature = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &tampered_signature,
        &data_image,
        &serial_log,
    )?;
    require_security_rejection(&changed_signature, "SignatureInvalid")?;

    println!(
        "security smoke passed: signed v2 booted; rollback, kernel tamper, and signature tamper were rejected"
    );
    Ok(())
}

fn smoke_trust_rotation() -> Result<()> {
    let root_identity = generate_signing_identity()?;
    let next_identity = generate_signing_identity()?;
    let loader =
        build_loader_with_trust_key(true, LoaderMode::Chainload, &root_identity.verifying_key)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;

    let rotation_release = build_dir.join("trust-rotation-v2.img");
    let next_release = build_dir.join("trust-rotation-v3.img");
    let old_root_release = build_dir.join("trust-rotation-old-root-v4.img");
    let changed_rotation = build_dir.join("trust-rotation-changed-next-key.img");
    make_image_at_with_identity(
        &rotation_release,
        &loader,
        &kernel,
        2,
        &root_identity,
        Some(&next_identity.verifying_key),
    )?;
    make_image_at_with_identity(&next_release, &loader, &kernel, 3, &next_identity, None)?;
    make_image_at_with_identity(&old_root_release, &loader, &kernel, 4, &root_identity, None)?;
    fs::copy(&rotation_release, &changed_rotation)
        .context("copying trust-root transition tamper test image")?;
    mutate_fat_file_byte(&changed_rotation, "SYSTEM/A/KERNEL.SIG", 80)?;
    mutate_fat_file_byte(&changed_rotation, "SYSTEM/B/KERNEL.SIG", 80)?;

    let next_key_id = key_id_for_public_key(next_identity.verifying_key.as_bytes());
    let next_key_id_hex = hex_encode(&next_key_id);
    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-trust-rotation.log");

    let tamper_vars = reset_trust_rotation_ovmf_vars("trust-rotation-tamper-ovmf-vars.fd")?;
    let tampered = launch_smoke_boot(
        &qemu,
        &ovmf,
        &tamper_vars,
        &changed_rotation,
        &data_image,
        &serial_log,
    )?;
    require_security_rejection(&tampered, "SignatureInvalid")?;

    let ovmf_vars = reset_trust_rotation_ovmf_vars("trust-rotation-ovmf-vars.fd")?;
    let rotated = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &rotation_release,
        &data_image,
        &serial_log,
    )?;
    let expected_activation = format!(
        "trust root update: activated version=2 previous-source=embedded next-key-id={next_key_id_hex}"
    );
    if !rotated.contains("kernel trust root: source=embedded")
        || !rotated.contains(&expected_activation)
        || !rotated.contains("kernel signature: verified=true version=2")
        || !rotated.contains("codexOS standalone kernel entered")
    {
        bail!(
            "trust-root transition release did not activate the next root; serial output:\n{rotated}"
        );
    }

    let next = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &next_release,
        &data_image,
        &serial_log,
    )?;
    let expected_persisted = format!(
        "kernel trust root: source=persisted activation-version=2 signer-key-id={next_key_id_hex}"
    );
    let expected_no_update =
        format!("trust root update: none source=persisted signer-key-id={next_key_id_hex}");
    if !next.contains(&expected_persisted)
        || !next.contains(&expected_no_update)
        || !next.contains("kernel signature: verified=true version=3")
        || !next.contains("codexOS standalone kernel entered")
    {
        bail!(
            "post-transition release did not boot from the persisted trust root; serial output:\n{next}"
        );
    }

    let old_root = launch_smoke_boot(
        &qemu,
        &ovmf,
        &ovmf_vars,
        &old_root_release,
        &data_image,
        &serial_log,
    )?;
    require_security_rejection(&old_root, "KeyIdMismatch")?;

    println!(
        "trust-root rotation smoke passed: v2 activated key {}, v3 booted from it, and old-root v4 was rejected",
        next_key_id_hex
    );
    Ok(())
}

fn smoke_network_listener() -> Result<()> {
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("network-listener.img");
    make_image_at(&image, &loader, &kernel, release_version()?)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_isolated_ovmf_vars("network-listener-ovmf-vars.fd")?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-network-listener.log");
    let packet_log = build_dir.join("network-listener.pcap");
    let _ = fs::remove_file(&serial_log);
    let _ = fs::remove_file(&packet_log);
    let host_port = reserve_local_tcp_port()?;

    let mut command = Command::new(qemu);
    command.arg("-machine").arg("q35");
    command.arg("-m").arg("512");
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    ));
    command
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", ovmf_vars.display()));
    command
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
    attach_persistent_data_disk(&mut command, &data_image);
    attach_user_network(
        &mut command,
        Some(HostForward {
            host_port,
            guest_port: NETWORK_LISTENER_GUEST_PORT,
        }),
    );
    command.arg("-object").arg(format!(
        "filter-dump,id=codexnetdump,netdev=codexnet,file={}",
        packet_log.display()
    ));
    command.arg("-display").arg("none");
    command
        .arg("-serial")
        .arg(format!("file:{}", serial_log.display()));
    command.arg("-monitor").arg("none");

    let mut child = command
        .spawn()
        .context("starting network listener smoke boot")?;
    let result = (|| {
        wait_for_serial_log(
            &serial_log,
            &format!("network tcp listener ready: port={NETWORK_LISTENER_GUEST_PORT}"),
            Duration::from_secs(30),
        )?;
        wait_for_serial_log(
            &serial_log,
            "standalone keyboard polling active",
            Duration::from_secs(30),
        )?;
        let response = probe_kernel_http_listener(host_port)?;
        if !response.starts_with("HTTP/1.1 200 OK") || !response.contains("codexOS listener online")
        {
            bail!("kernel listener returned an unexpected HTTP response:\n{response}");
        }
        let served = wait_for_serial_log(
            &serial_log,
            "tcp listener served: port=8080",
            Duration::from_secs(10),
        )?;
        validate_network_listener_log(&served)?;
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result?;

    println!(
        "network listener smoke passed: host 127.0.0.1:{} reached guest port {} and received HTTP 200",
        host_port, NETWORK_LISTENER_GUEST_PORT
    );
    Ok(())
}

fn smoke_pointer_input() -> Result<()> {
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("pointer-input.img");
    make_image_at(&image, &loader, &kernel, release_version()?)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_isolated_ovmf_vars("pointer-input-ovmf-vars.fd")?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-pointer-input.log");
    let _ = fs::remove_file(&serial_log);
    let monitor_port = reserve_local_tcp_port()?;

    let mut command = Command::new(qemu);
    command.arg("-machine").arg("q35");
    command.arg("-m").arg("512");
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    ));
    command
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", ovmf_vars.display()));
    command
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
    attach_persistent_data_disk(&mut command, &data_image);
    attach_user_network(&mut command, None);
    command.arg("-display").arg("none");
    command
        .arg("-serial")
        .arg(format!("file:{}", serial_log.display()));
    command
        .arg("-monitor")
        .arg(format!("tcp:127.0.0.1:{monitor_port},server,nowait"));

    let mut child = command
        .spawn()
        .context("starting pointer input smoke boot")?;
    let result = (|| {
        let pointer_ready = wait_for_serial_log(
            &serial_log,
            "standalone pointer polling active: device=ps2",
            Duration::from_secs(30),
        )?;
        if !pointer_ready.contains("standalone pointer polling active: device=ps2 enabled=true") {
            bail!("PS/2 pointer device did not enable; serial output:\n{pointer_ready}");
        }

        let mut monitor = connect_hmp_monitor(monitor_port)?;
        send_hmp_command(&mut monitor, "mouse_move 32 -16")?;
        let log = wait_for_serial_log(
            &serial_log,
            "pointer input event: device=ps2",
            Duration::from_secs(10),
        )?;
        validate_pointer_event_log(&log)?;
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result?;

    println!(
        "pointer input smoke passed: QEMU monitor movement reached the resident PS/2 pointer driver"
    );
    Ok(())
}

fn smoke_gpt_esp() -> Result<()> {
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("gpt-esp-chainload.img");
    make_gpt_image_at(&image, &loader, &kernel, release_version()?)?;
    validate_gpt_esp_image(&image)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_isolated_ovmf_vars("gpt-esp-ovmf-vars.fd")?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-gpt-esp.log");
    let log = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !log.contains("kernel signature: verified=true version=1 slot=A")
        || !log.contains("codexOS standalone kernel entered")
        || !log.contains("network tcp listener ready: port=8080 protocol=http")
    {
        bail!(
            "GPT/ESP smoke boot did not reach the signed chainload kernel; serial output:\n{log}"
        );
    }
    println!("gpt esp smoke passed: signed chainload image booted from an EFI System Partition");
    Ok(())
}

fn smoke_recovery() -> Result<()> {
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("recovery-fallback.img");
    make_image_at(&image, &loader, &kernel, 1)?;
    mutate_fat_file_byte(&image, "SYSTEM/A/KERNEL.ELF", 0)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_security_ovmf_vars()?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-recovery.log");
    let log = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !log.contains("system slot A rejected: KernelHashMismatch")
        || !log.contains("system recovery fallback: active=A selected=B version=1")
        || !log
            .contains("kernel signature: verified=true version=1 slot=B state-gen=2 recovery=true")
        || !log.contains("codexOS standalone kernel entered")
    {
        bail!("recovery smoke did not boot the signed fallback slot; serial output:\n{log}");
    }
    println!(
        "recovery smoke passed: corrupted active slot A was rejected and signed slot B booted"
    );
    Ok(())
}

fn smoke_boot_state_recovery() -> Result<()> {
    let loader = build_loader(true, LoaderMode::Chainload)?;
    let kernel = build_kernel_image(true)?;
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("bootstate-recovery.img");
    make_image_at(&image, &loader, &kernel, 1)?;
    mutate_fat_file_byte(&image, "BOOTSTA0.BIN", 0)?;
    mutate_fat_file_byte(&image, "BOOTSTA1.BIN", 0)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_isolated_ovmf_vars("bootstate-recovery-ovmf-vars.fd")?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-bootstate-recovery.log");
    let log = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !log.contains("system boot-state recovery: selected=A version=1 source=signed-slot-scan")
        || !log
            .contains("kernel signature: verified=true version=1 slot=A state-gen=0 recovery=true")
        || !log.contains("codexOS standalone kernel entered")
        || !log.contains("standalone pointer polling active: device=ps2 enabled=true")
    {
        bail!(
            "boot-state recovery smoke did not recover from damaged boot-state records; serial output:\n{log}"
        );
    }
    println!(
        "boot-state recovery smoke passed: damaged boot-state records recovered through signed slot scan"
    );
    Ok(())
}

fn smoke_install_and_update() -> Result<()> {
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("smoke-installed-system.img");
    if image.exists() {
        fs::remove_file(&image)
            .with_context(|| format!("removing previous smoke image {}", image.display()))?;
    }
    install_system(&image)?;
    apply_signed_update_version(&image, 2)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_security_ovmf_vars()?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-install.log");
    let installed = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !installed
        .contains("kernel signature: verified=true version=2 slot=B state-gen=3 recovery=false")
        || !installed.contains("codexOS standalone kernel entered")
    {
        bail!("installed update did not boot slot B release 2; serial output:\n{installed}");
    }

    mutate_fat_file_byte(&image, "SYSTEM/B/KERNEL.ELF", 0)?;
    let recovered = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !recovered.contains("system slot B rejected: KernelHashMismatch")
        || !recovered.contains("system recovery fallback: active=B selected=A version=2")
        || !recovered
            .contains("kernel signature: verified=true version=2 slot=A state-gen=3 recovery=true")
        || !recovered.contains("codexOS standalone kernel entered")
    {
        bail!("updated system did not recover from slot B corruption; serial output:\n{recovered}");
    }
    println!(
        "install smoke passed: readback-verified install, atomic v2 slot switch, and v2 fallback recovery all booted"
    );
    Ok(())
}

fn smoke_gpt_install_and_update() -> Result<()> {
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;
    let image = build_dir.join("smoke-gpt-installed-system.img");
    if image.exists() {
        fs::remove_file(&image)
            .with_context(|| format!("removing previous smoke image {}", image.display()))?;
    }
    install_gpt_system(&image)?;
    apply_signed_update_version(&image, 2)?;
    validate_gpt_esp_image(&image)?;

    let qemu = discover_qemu()?;
    let ovmf = discover_ovmf()?;
    let ovmf_vars = reset_isolated_ovmf_vars("gpt-install-ovmf-vars.fd")?;
    let data_image = persistent_data_image()?;
    let serial_log = build_dir.join("serial-gpt-install.log");
    let installed = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !installed
        .contains("kernel signature: verified=true version=2 slot=B state-gen=3 recovery=false")
        || !installed.contains("codexOS standalone kernel entered")
        || !installed.contains("network tcp listener ready: port=8080 protocol=http")
    {
        bail!("GPT installed update did not boot slot B release 2; serial output:\n{installed}");
    }

    mutate_fat_file_byte(&image, "SYSTEM/B/KERNEL.ELF", 0)?;
    let recovered = launch_smoke_boot(&qemu, &ovmf, &ovmf_vars, &image, &data_image, &serial_log)?;
    if !recovered.contains("system slot B rejected: KernelHashMismatch")
        || !recovered.contains("system recovery fallback: active=B selected=A version=2")
        || !recovered
            .contains("kernel signature: verified=true version=2 slot=A state-gen=3 recovery=true")
        || !recovered.contains("codexOS standalone kernel entered")
    {
        bail!(
            "GPT updated system did not recover from slot B corruption; serial output:\n{recovered}"
        );
    }
    println!(
        "gpt install smoke passed: GPT/ESP install, atomic v2 slot switch, and v2 fallback recovery all booted"
    );
    Ok(())
}

fn require_security_rejection(log: &str, reason: &str) -> Result<()> {
    if !log.contains("kernel security failure:") || !log.contains(reason) {
        bail!("security smoke did not report {reason}; serial output:\n{log}");
    }
    if log.contains("codexOS kernel entered") || log.contains("codexOS standalone kernel entered") {
        bail!("security smoke entered an untrusted kernel after reporting {reason}");
    }
    Ok(())
}

fn mutate_fat_file_byte(image: &Path, name: &str, offset: u64) -> Result<()> {
    let disk = OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .with_context(|| format!("opening {} for security mutation", image.display()))?;
    match detect_installed_layout(&disk)? {
        InstalledLayout::WholeDiskFat => {
            let mut volume = disk
                .try_clone()
                .with_context(|| format!("cloning image handle {}", image.display()))?;
            volume
                .seek(SeekFrom::Start(0))
                .with_context(|| format!("rewinding image {}", image.display()))?;
            let filesystem = FileSystem::new(volume, FsOptions::new())
                .with_context(|| format!("opening FAT filesystem in {}", image.display()))?;
            mutate_fat_file_byte_in_filesystem(filesystem, image, name, offset)?;
        }
        InstalledLayout::GptEsp(esp) => {
            let volume = OffsetFile::new(
                disk.try_clone()?,
                esp.first_lba * DISK_SECTOR_BYTES,
                esp.sector_count * DISK_SECTOR_BYTES,
            )?;
            let filesystem = FileSystem::new(volume, FsOptions::new()).with_context(|| {
                format!("opening GPT EFI System Partition in {}", image.display())
            })?;
            mutate_fat_file_byte_in_filesystem(filesystem, image, name, offset)?;
        }
    }
    disk.sync_all()
        .with_context(|| format!("flushing mutated image {}", image.display()))?;
    Ok(())
}

fn mutate_fat_file_byte_in_filesystem<T: Read + Write + Seek>(
    filesystem: FileSystem<T>,
    image: &Path,
    name: &str,
    offset: u64,
) -> Result<()> {
    {
        let root = filesystem.root_dir();
        let mut file = root
            .open_file(name)
            .with_context(|| format!("opening {name} in {}", image.display()))?;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("seeking {name} to byte {offset}"))?;
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte)
            .with_context(|| format!("reading {name} byte {offset}"))?;
        byte[0] ^= 0x80;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("rewinding {name} to byte {offset}"))?;
        file.write_all(&byte)
            .with_context(|| format!("mutating {name} byte {offset}"))?;
        file.flush().with_context(|| format!("flushing {name}"))?;
    }
    filesystem
        .unmount()
        .with_context(|| format!("unmounting filesystem in {}", image.display()))?;
    Ok(())
}

fn reset_security_ovmf_vars() -> Result<PathBuf> {
    reset_isolated_ovmf_vars("security-ovmf-vars.fd")
}

fn reset_trust_rotation_ovmf_vars(name: &str) -> Result<PathBuf> {
    reset_isolated_ovmf_vars(name)
}

fn reset_isolated_ovmf_vars(name: &str) -> Result<PathBuf> {
    let template = discover_ovmf_vars()?;
    let path = workspace_root().join("build").join(name);
    fs::copy(&template, &path).with_context(|| {
        format!(
            "resetting isolated security variable store {} from {}",
            path.display(),
            template.display()
        )
    })?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening isolated variable store {}", path.display()))?
        .sync_all()
        .with_context(|| format!("flushing isolated variable store {}", path.display()))?;
    Ok(path)
}

fn launch_smoke_boot(
    qemu: &Path,
    ovmf: &Path,
    ovmf_vars: &Path,
    image: &Path,
    data_image: &Path,
    serial_log: &Path,
) -> Result<String> {
    let _ = fs::remove_file(serial_log);
    let mut command = Command::new(qemu);
    command.arg("-machine").arg("q35");
    command.arg("-m").arg("512");
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    ));
    command
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", ovmf_vars.display()));
    command
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
    attach_persistent_data_disk(&mut command, data_image);
    attach_user_network(&mut command, None);
    command.arg("-display").arg("none");
    command
        .arg("-serial")
        .arg(format!("file:{}", serial_log.display()));
    command.arg("-monitor").arg("none");

    let mut child = command
        .spawn()
        .context("starting headless QEMU smoke boot")?;
    thread::sleep(Duration::from_secs(8));
    let _ = child.kill();
    let _ = child.wait();
    fs::read_to_string(serial_log).with_context(|| format!("reading {}", serial_log.display()))
}

fn reserve_local_tcp_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("reserving a local TCP port for QEMU forwarding")?;
    let port = listener
        .local_addr()
        .context("reading reserved local TCP port")?
        .port();
    drop(listener);
    Ok(port)
}

fn wait_for_serial_log(path: &Path, needle: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        if let Ok(log) = fs::read_to_string(path) {
            if log.contains(needle) {
                return Ok(log);
            }
            last = log;
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "serial log did not contain {needle:?} within {:?}; serial output:\n{}",
        timeout,
        last
    )
}

fn probe_kernel_http_listener(host_port: u16) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut last_error: Option<anyhow::Error> = None;
    while Instant::now() < deadline {
        match TcpStream::connect(("127.0.0.1", host_port)) {
            Ok(mut stream) => {
                let attempt = (|| {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .context("setting listener probe read timeout")?;
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .context("setting listener probe write timeout")?;
                    stream
                        .write_all(
                            b"GET / HTTP/1.1\r\nHost: codexos.local\r\nConnection: close\r\n\r\n",
                        )
                        .context("writing HTTP request to kernel listener")?;
                    stream
                        .shutdown(std::net::Shutdown::Write)
                        .context("closing listener probe request stream")?;
                    let mut response = String::new();
                    stream
                        .read_to_string(&mut response)
                        .context("reading HTTP response from kernel listener")?;
                    Ok(response)
                })();
                match attempt {
                    Ok(response) => return Ok(response),
                    Err(error) => {
                        last_error = Some(error);
                        thread::sleep(Duration::from_millis(250));
                    }
                }
            }
            Err(error) => {
                last_error = Some(error.into());
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    match last_error {
        Some(error) => Err(error).context("connecting to forwarded kernel listener"),
        None => bail!("connecting to forwarded kernel listener timed out"),
    }
}

fn connect_hmp_monitor(port: u16) -> Result<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_error: Option<anyhow::Error> = None;
    while Instant::now() < deadline {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .context("setting QEMU monitor read timeout")?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .context("setting QEMU monitor write timeout")?;
                read_hmp_until_prompt(&mut stream).context("reading QEMU monitor greeting")?;
                return Ok(stream);
            }
            Err(error) => {
                last_error = Some(error.into());
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    match last_error {
        Some(error) => Err(error).context("connecting to QEMU monitor"),
        None => bail!("connecting to QEMU monitor timed out"),
    }
}

fn send_hmp_command(stream: &mut TcpStream, command: &str) -> Result<String> {
    stream
        .write_all(command.as_bytes())
        .with_context(|| format!("writing QEMU monitor command {command}"))?;
    stream
        .write_all(b"\n")
        .with_context(|| format!("terminating QEMU monitor command {command}"))?;
    stream
        .flush()
        .with_context(|| format!("flushing QEMU monitor command {command}"))?;
    let response = read_hmp_until_prompt(stream)
        .with_context(|| format!("reading QEMU monitor response for {command}"))?;
    let lower = response.to_ascii_lowercase();
    if lower.contains("unknown command") || lower.contains("invalid parameter") {
        bail!("QEMU monitor rejected {command:?}: {response}");
    }
    Ok(response)
}

fn read_hmp_until_prompt(stream: &mut TcpStream) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut output = Vec::new();
    let mut buffer = [0_u8; 512];
    while Instant::now() < deadline {
        match stream.read(&mut buffer) {
            Ok(0) => bail!("QEMU monitor closed the TCP connection"),
            Ok(count) => {
                output.extend_from_slice(&buffer[..count]);
                if output
                    .windows(b"(qemu)".len())
                    .any(|window| window == b"(qemu)")
                {
                    return Ok(String::from_utf8_lossy(&output).into_owned());
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error).context("reading from QEMU monitor"),
        }
    }
    bail!(
        "QEMU monitor did not return a prompt; response so far:\n{}",
        String::from_utf8_lossy(&output)
    )
}

fn persistence_counter(log: &str) -> Result<(u64, u64)> {
    let line = log
        .lines()
        .find(|line| line.contains("filesystem persistence:"))
        .context("serial log did not contain a filesystem persistence result")?;
    if !line.contains("verified=true") {
        bail!("filesystem did not report a verified commit: {line}");
    }
    let mut previous = None;
    let mut current = None;
    let mut directories = None;
    for field in line.split_whitespace() {
        if let Some(value) = field.strip_prefix("previous=") {
            previous = Some(
                value
                    .parse::<u64>()
                    .context("parsing previous boot counter")?,
            );
        } else if let Some(value) = field.strip_prefix("current=") {
            current = Some(
                value
                    .parse::<u64>()
                    .context("parsing current boot counter")?,
            );
        } else if let Some(value) = field.strip_prefix("directories=") {
            directories = Some(
                value
                    .parse::<u64>()
                    .context("parsing filesystem directory count")?,
            );
        }
    }
    if directories.unwrap_or(0) < 2 {
        bail!("filesystem namespace did not contain the required directories: {line}");
    }
    Ok((
        previous.context("persistence result omitted previous boot counter")?,
        current.context("persistence result omitted current boot counter")?,
    ))
}

fn validate_filesystem_log(log: &str) -> Result<()> {
    let mount = log
        .lines()
        .find(|line| line.contains("codexfs: mounted state="))
        .context("serial log omitted the CodexFS mount report")?;
    for required in ["slot-sectors=", "active-sectors=", "max-record-bytes="] {
        if !mount.contains(required) {
            bail!("CodexFS mount report missing {required}: {mount}");
        }
    }
    let slot_sectors = parse_log_usize_field(mount, "slot-sectors=")?;
    let max_record_bytes = parse_log_usize_field(mount, "max-record-bytes=")?;
    if slot_sectors <= 64 || max_record_bytes <= 64 * 512 {
        bail!("CodexFS mount report did not expose dynamic record capacity: {mount}");
    }

    let line = log
        .lines()
        .find(|line| line.contains("filesystem large-file verified:"))
        .context("serial log omitted the CodexFS large-file verification report")?;
    for required in [
        "path=/system/large-proof.bin",
        "bytes=98304",
        "checksum=",
        "record-sectors=",
        "slot-sectors=",
        "written=",
    ] {
        if !line.contains(required) {
            bail!("CodexFS large-file report missing {required}: {line}");
        }
    }
    let bytes = parse_log_usize_field(line, "bytes=")?;
    let record_sectors = parse_log_usize_field(line, "record-sectors=")?;
    let slot_sectors = parse_log_usize_field(line, "slot-sectors=")?;
    if bytes != 96 * 1024 || record_sectors <= 64 || slot_sectors < record_sectors {
        bail!("CodexFS large-file report did not prove a dynamic multisector commit: {line}");
    }
    let expected_checksum = format!("checksum=0x{:08x}", large_file_proof_checksum());
    if !line.contains(&expected_checksum) {
        bail!("CodexFS large-file report checksum mismatch: {line}");
    }
    Ok(())
}

fn large_file_proof_checksum() -> u32 {
    let mut checksum = 0_u32;
    for index in 0..(96 * 1024_usize) {
        let byte = ((index.wrapping_mul(31).wrapping_add(index / 251)) & 0xff) as u8;
        checksum = checksum.rotate_left(5) ^ u32::from(byte);
    }
    checksum
}

fn validate_persistent_executable_log(log: &str) -> Result<()> {
    let line = log
        .lines()
        .find(|line| line.contains("persistent executable verified:"))
        .context("serial log omitted the persistent executable verification result")?;
    for required in [
        "path=/system/bin/scheduler-gate.elf",
        "segments=1",
        "exit=2001",
        "idle-halts=",
        "reclaimed-pages=6",
        "sha256-prefix=",
    ] {
        if !line.contains(required) {
            bail!("persistent executable verification result missing {required}: {line}");
        }
    }
    Ok(())
}

fn validate_hardware_log(log: &str) -> Result<()> {
    let line = log
        .lines()
        .find(|line| line.contains("hardware inventory:"))
        .context("serial log omitted the hardware inventory report")?;
    for required in [
        "overflow=false",
        "virtio-blk=",
        "virtio-net=",
        "virtio-legacy=",
        "io-bars=",
    ] {
        if !line.contains(required) {
            bail!("hardware inventory report missing {required}: {line}");
        }
    }

    let pci_devices = parse_log_usize_field(line, "pci-devices=")?;
    let storage = parse_log_usize_field(line, "storage=")?;
    let network = parse_log_usize_field(line, "network=")?;
    let virtio_legacy = parse_log_usize_field(line, "virtio-legacy=")?;
    let io_bars = parse_log_usize_field(line, "io-bars=")?;
    if pci_devices < 2 || storage < 1 || network < 1 || virtio_legacy < 2 || io_bars < 2 {
        bail!("hardware inventory did not prove boot storage and network devices: {line}");
    }

    for device in ["virtio-blk", "virtio-net"] {
        let expected = format!("hardware driver binding: device={device} ");
        let bound = log.lines().any(|line| {
            line.contains(&expected)
                && line.contains("driver=ready")
                && line.contains("inventory-match=true")
        });
        if !bound {
            bail!("hardware inventory did not bind a ready {device} driver; serial output:\n{log}");
        }
    }

    Ok(())
}

fn parse_log_usize_field(line: &str, prefix: &str) -> Result<usize> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(prefix))
        .with_context(|| format!("log line omitted {prefix}"))?
        .parse::<usize>()
        .with_context(|| format!("parsing log field {prefix}"))
}

fn parse_log_i32_field(line: &str, prefix: &str) -> Result<i32> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(prefix))
        .with_context(|| format!("log line omitted {prefix}"))?
        .parse::<i32>()
        .with_context(|| format!("parsing log field {prefix}"))
}

fn validate_network_log(log: &str) -> Result<()> {
    if !log.contains("virtio-net: pci=")
        || !log.contains(
            "network configured: ipv4=10.0.2.15 mask=255.255.255.0 gateway=10.0.2.2 dhcp=10.0.2.2",
        )
        || !log.contains("arp gateway verified: ip=10.0.2.2 mac=")
        || !log.contains("icmp echo verified: destination=10.0.2.2 sequence=1")
        || !log.contains("dns resolved: name=example.com server=")
        || !log.contains("tcp http verified: host=example.com remote=")
        || !log.contains("network tcp listener ready: port=8080 protocol=http")
    {
        bail!(
            "chainload smoke boot did not complete DHCP, ARP, ICMP, DNS, active TCP/HTTP, and listener setup; serial output:\n{log}"
        );
    }
    let line = log
        .lines()
        .find(|line| line.contains("network configured:"))
        .context("serial log omitted the network configuration report")?;
    let mut lease = None;
    let mut transmitted = None;
    let mut received = None;
    for field in line.split_whitespace() {
        if let Some(value) = field
            .strip_prefix("lease=")
            .and_then(|value| value.strip_suffix('s'))
        {
            lease = Some(
                value
                    .parse::<u32>()
                    .context("parsing DHCP lease duration")?,
            );
        } else if let Some(value) = field.strip_prefix("tx=") {
            transmitted = Some(
                value
                    .parse::<u64>()
                    .context("parsing transmitted frame count")?,
            );
        } else if let Some(value) = field.strip_prefix("rx=") {
            received = Some(
                value
                    .parse::<u64>()
                    .context("parsing received frame count")?,
            );
        }
    }
    if lease.unwrap_or(0) == 0 || transmitted.unwrap_or(0) < 8 || received.unwrap_or(0) < 7 {
        bail!(
            "network report did not prove a complete bidirectional DHCP/ARP/DNS/TCP exchange: {line}"
        );
    }
    let dns = log
        .lines()
        .find(|line| line.contains("dns resolved:"))
        .context("serial log omitted the DNS resolution report")?;
    if !dns.contains("query-id=0x4344") {
        bail!("DNS report used an unexpected query id: {dns}");
    }
    let answer = dns
        .split_whitespace()
        .find_map(|field| field.strip_prefix("answer="))
        .context("DNS report omitted the A record answer")?;
    if answer == "0.0.0.0" {
        bail!("DNS report returned an unusable A record: {dns}");
    }
    let http = log
        .lines()
        .find(|line| line.contains("tcp http verified:"))
        .context("serial log omitted the TCP/HTTP verification report")?;
    if !http.contains("source-port=49153") {
        bail!("TCP/HTTP report used an unexpected source port: {http}");
    }
    let status = http
        .split_whitespace()
        .find_map(|field| field.strip_prefix("status="))
        .context("TCP/HTTP report omitted the HTTP status")?
        .parse::<u16>()
        .context("parsing HTTP status")?;
    let bytes = http
        .split_whitespace()
        .find_map(|field| field.strip_prefix("bytes="))
        .context("TCP/HTTP report omitted response bytes")?
        .parse::<usize>()
        .context("parsing HTTP response byte count")?;
    if !(100..600).contains(&status) || bytes == 0 {
        bail!("TCP/HTTP report did not prove a usable HTTP response: {http}");
    }
    Ok(())
}

fn validate_network_listener_log(log: &str) -> Result<()> {
    let ready = log
        .lines()
        .find(|line| line.contains("network tcp listener ready:"))
        .context("serial log omitted the TCP listener readiness report")?;
    if !ready.contains("port=8080") || !ready.contains("protocol=http") {
        bail!("TCP listener readiness report was incomplete: {ready}");
    }

    let served = log
        .lines()
        .find(|line| line.contains("tcp listener served:"))
        .context("serial log omitted the TCP listener served report")?;
    for required in [
        "port=8080",
        "remote=",
        "request-bytes=",
        "response-bytes=",
        "connections=",
        "tx=",
        "rx=",
    ] {
        if !served.contains(required) {
            bail!("TCP listener served report missing {required}: {served}");
        }
    }
    let request_bytes = parse_log_usize_field(served, "request-bytes=")?;
    let response_bytes = parse_log_usize_field(served, "response-bytes=")?;
    let connections = parse_log_usize_field(served, "connections=")?;
    if request_bytes < 16 || response_bytes < 64 || connections == 0 {
        bail!("TCP listener served report did not prove an HTTP exchange: {served}");
    }
    Ok(())
}

fn validate_pointer_driver_log(log: &str) -> Result<()> {
    let line = log
        .lines()
        .find(|line| line.contains("standalone pointer polling active: device=ps2"))
        .context("serial log omitted the resident PS/2 pointer driver report")?;
    if !line.contains("enabled=true") || !line.contains("id=") {
        bail!("resident PS/2 pointer driver did not report an enabled device: {line}");
    }
    let acknowledgements = parse_log_usize_field(line, "acknowledgements=")?;
    if acknowledgements < 2 {
        bail!("resident PS/2 pointer driver did not complete mouse command handshakes: {line}");
    }
    Ok(())
}

fn validate_pointer_event_log(log: &str) -> Result<()> {
    let line = log
        .lines()
        .find(|line| line.contains("pointer input event: device=ps2"))
        .context("serial log omitted the PS/2 pointer event report")?;
    for required in ["dx=", "dy=", "left=", "right="] {
        if !line.contains(required) {
            bail!("PS/2 pointer event report missing {required}: {line}");
        }
    }
    let dx = parse_log_i32_field(line, "dx=")?;
    let dy = parse_log_i32_field(line, "dy=")?;
    if dx == 0 && dy == 0 {
        bail!("PS/2 pointer event did not carry movement: {line}");
    }
    Ok(())
}

fn has_applied_kernel_relocations(log: &str) -> bool {
    log.lines().any(|line| {
        line.split_once("relocs=")
            .and_then(|(_, value)| value.split_whitespace().next())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|count| count != 0)
    })
}

fn persistent_data_image() -> Result<PathBuf> {
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir).context("creating build directory for persistent storage")?;
    let path = build_dir.join("codexos-data.img");
    if path.exists() {
        let size = fs::metadata(&path)
            .with_context(|| format!("reading persistent disk metadata from {}", path.display()))?
            .len();
        if size != DATA_IMAGE_SIZE_BYTES {
            bail!(
                "persistent disk {} has size {} bytes; expected {} bytes; refusing to resize a disk that may contain data",
                path.display(),
                size,
                DATA_IMAGE_SIZE_BYTES
            );
        }
        return Ok(path);
    }

    let disk = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("creating persistent disk {}", path.display()))?;
    disk.set_len(DATA_IMAGE_SIZE_BYTES)
        .with_context(|| format!("sizing persistent disk {}", path.display()))?;
    disk.sync_all()
        .with_context(|| format!("flushing persistent disk {}", path.display()))?;
    Ok(path)
}

fn persistent_ovmf_vars() -> Result<PathBuf> {
    let template = discover_ovmf_vars()?;
    let expected_size = fs::metadata(&template)
        .with_context(|| format!("reading OVMF variable template {}", template.display()))?
        .len();
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir).context("creating build directory for firmware variables")?;
    let path = build_dir.join("codexos-ovmf-vars.fd");
    if path.exists() {
        let size = fs::metadata(&path)
            .with_context(|| format!("reading firmware variable store {}", path.display()))?
            .len();
        if size != expected_size {
            bail!(
                "firmware variable store {} has size {} bytes; expected {} bytes; refusing to replace persistent rollback state",
                path.display(),
                size,
                expected_size
            );
        }
        return Ok(path);
    }
    fs::copy(&template, &path).with_context(|| {
        format!(
            "initializing firmware variable store {} from {}",
            path.display(),
            template.display()
        )
    })?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening firmware variable store {}", path.display()))?
        .sync_all()
        .with_context(|| format!("flushing firmware variable store {}", path.display()))?;
    Ok(path)
}

fn attach_persistent_data_disk(command: &mut Command, data_image: &Path) {
    command.arg("-drive").arg(format!(
        "if=none,id=codexdata,format=raw,file={}",
        data_image.display()
    ));
    command
        .arg("-device")
        .arg("virtio-blk-pci,drive=codexdata,disable-modern=on");
}

#[derive(Clone, Copy)]
struct HostForward {
    host_port: u16,
    guest_port: u16,
}

fn attach_user_network(command: &mut Command, host_forward: Option<HostForward>) {
    let netdev = if let Some(forward) = host_forward {
        format!(
            "user,id=codexnet,hostfwd=tcp:127.0.0.1:{}-10.0.2.15:{}",
            forward.host_port, forward.guest_port
        )
    } else {
        String::from("user,id=codexnet")
    };
    command.arg("-netdev").arg(netdev);
    command
        .arg("-device")
        .arg("virtio-net-pci,netdev=codexnet,disable-modern=on,mac=52:54:00:12:34:56");
}

fn print_env() -> Result<()> {
    println!("workspace : {}", workspace_root().display());
    println!("qemu      : {}", discover_qemu()?.display());
    println!("ovmf      : {}", discover_ovmf()?.display());
    println!("ovmf vars : {}", persistent_ovmf_vars()?.display());
    println!("data disk : {}", persistent_data_image()?.display());
    Ok(())
}

fn discover_qemu() -> Result<PathBuf> {
    if let Ok(path) = env::var("CODEXOS_QEMU") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!("CODEXOS_QEMU does not point to a file: {}", path.display());
    }

    if let Ok(path) = which("qemu-system-x86_64") {
        return Ok(path);
    }

    let candidates = [
        r"D:\Program Files\qemu\qemu-system-x86_64.exe",
        r"C:\Program Files\qemu\qemu-system-x86_64.exe",
    ];
    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }

    bail!("could not find qemu-system-x86_64; set CODEXOS_QEMU to the executable path")
}

fn discover_ovmf() -> Result<PathBuf> {
    if let Ok(path) = env::var("CODEXOS_OVMF") {
        return Ok(PathBuf::from(path));
    }

    let candidates = [
        r"D:\Program Files\qemu\share\edk2-x86_64-code.fd",
        r"C:\Program Files\qemu\share\edk2-x86_64-code.fd",
    ];

    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Ok(path);
        }
    }

    bail!("could not find edk2-x86_64-code.fd; set CODEXOS_OVMF to the firmware path")
}

fn discover_ovmf_vars() -> Result<PathBuf> {
    if let Ok(path) = env::var("CODEXOS_OVMF_VARS") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "CODEXOS_OVMF_VARS does not point to a file: {}",
            path.display()
        );
    }
    let candidates = [
        r"D:\Program Files\qemu\share\edk2-i386-vars.fd",
        r"C:\Program Files\qemu\share\edk2-i386-vars.fd",
    ];
    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("could not find edk2-i386-vars.fd; set CODEXOS_OVMF_VARS to the template path")
}

fn which(binary: &str) -> Result<PathBuf> {
    let output = Command::new("where")
        .arg(binary)
        .output()
        .with_context(|| format!("resolving {binary} with `where`"))?;
    if !output.status.success() {
        bail!("`where {binary}` failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout
        .lines()
        .next()
        .context("`where` returned no result")?;
    Ok(PathBuf::from(first.trim()))
}

fn run(mut command: Command, context: &str) -> Result<()> {
    let status = command.status().with_context(|| context.to_string())?;
    ensure_success(status, context)
}

fn ensure_success(status: ExitStatus, context: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{context} failed with status {status}");
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}
