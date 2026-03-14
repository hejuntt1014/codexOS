use anyhow::{Context, Result, bail};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::Duration;

const EFI_TARGET: &str = "x86_64-unknown-uefi";
const KERNEL_TARGET: &str = "x86_64-unknown-none";
const LOADER_BIN: &str = "uefi-loader.efi";
const KERNEL_BIN: &str = "kernel-image";
const IMAGE_SIZE_BYTES: u64 = 256 * 1024 * 1024;

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
            let loader = build_loader(false, LoaderMode::Interactive)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Interactive)?;
            println!("disk image ready: {}", image.display());
        }
        Some("image-handoff") => {
            let loader = build_loader(false, LoaderMode::Handoff)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Handoff)?;
            println!("disk image ready: {}", image.display());
        }
        Some("image-chainload") => {
            let loader = build_loader(false, LoaderMode::Chainload)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Chainload)?;
            println!("disk image ready: {}", image.display());
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
            let loader = build_loader(false, LoaderMode::Interactive)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Interactive)?;
            smoke_qemu(&image, LoaderMode::Interactive)?;
        }
        Some("smoke-handoff") => {
            let loader = build_loader(false, LoaderMode::Handoff)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Handoff)?;
            smoke_qemu(&image, LoaderMode::Handoff)?;
        }
        Some("smoke-chainload") => {
            let loader = build_loader(false, LoaderMode::Chainload)?;
            let kernel = build_kernel_image(true)?;
            let image = make_image(&loader, &kernel, LoaderMode::Chainload)?;
            smoke_qemu(&image, LoaderMode::Chainload)?;
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
    println!("  cargo xtask image          - pack an interactive FAT disk image");
    println!("  cargo xtask image-handoff  - pack a handoff FAT disk image");
    println!("  cargo xtask image-chainload - pack a chainload FAT disk image");
    println!("  cargo xtask run            - launch the interactive desktop in QEMU");
    println!("  cargo xtask handoff        - launch the handoff desktop in QEMU");
    println!("  cargo xtask chainload      - launch the standalone kernel chainload path in QEMU");
    println!("  cargo xtask debug          - launch QEMU paused with a gdb stub on :1234");
    println!("  cargo xtask smoke          - verify the interactive kernel log");
    println!("  cargo xtask smoke-handoff  - verify the post-EBS kernel log");
    println!("  cargo xtask smoke-chainload - verify the standalone kernel chainload log");
    println!("  cargo xtask env            - print discovered tool paths");
}

fn build_loader(release: bool, mode: LoaderMode) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command.arg("build");
    command.arg("-p").arg("uefi-loader");
    command.arg("--target").arg(EFI_TARGET);
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
    let output_dir = workspace_root().join("target").join(KERNEL_TARGET).join(profile);
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
    let image = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&image_path)
        .with_context(|| format!("creating {}", image_path.display()))?;
    image
        .set_len(IMAGE_SIZE_BYTES)
        .with_context(|| format!("resizing {}", image_path.display()))?;

    format_disk(image.try_clone()?)?;
    copy_artifacts_into_image(image, loader, kernel_image)?;

    Ok(image_path)
}

fn format_disk(mut image: File) -> Result<()> {
    image.seek(SeekFrom::Start(0))?;
    fatfs::format_volume(&mut image, FormatVolumeOptions::new())
        .context("formatting FAT volume")?;
    Ok(())
}

fn copy_artifacts_into_image(image: File, loader: &Path, kernel_image: &Path) -> Result<()> {
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

    let kernel_bytes =
        fs::read(kernel_image).with_context(|| format!("reading {}", kernel_image.display()))?;
    let mut kernel = root
        .create_file("KERNEL.ELF")
        .context("creating KERNEL.ELF")?;
    kernel
        .write_all(&kernel_bytes)
        .context("writing kernel ELF into the image")?;
    kernel.flush().context("flushing KERNEL.ELF")?;
    Ok(())
}

fn create_dir_if_missing(dir: &fatfs::Dir<'_, File>, name: &str) -> Result<()> {
    match dir.create_dir(name) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err).with_context(|| format!("creating directory {name}")),
    }
}

fn run_qemu(image: &Path, debug_wait: bool) -> Result<()> {
    let qemu = env::var("CODEXOS_QEMU")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("qemu-system-x86_64"));
    let ovmf = discover_ovmf()?;

    let mut command = Command::new(qemu);
    command.arg("-machine").arg("q35");
    command.arg("-m").arg("512");
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    ));
    command
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
    command.arg("-serial").arg("stdio");
    command.arg("-monitor").arg("none");

    if debug_wait {
        command.arg("-s");
        command.arg("-S");
    }

    run(command, "running QEMU")
}

fn smoke_qemu(image: &Path, mode: LoaderMode) -> Result<()> {
    let qemu = env::var("CODEXOS_QEMU")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("qemu-system-x86_64"));
    let ovmf = discover_ovmf()?;
    let serial_log = workspace_root().join("build").join(match mode {
        LoaderMode::Interactive => "serial-interactive.log",
        LoaderMode::Handoff => "serial-handoff.log",
        LoaderMode::Chainload => "serial-chainload.log",
    });
    let _ = fs::remove_file(&serial_log);

    let mut command = Command::new(qemu);
    command.arg("-machine").arg("q35");
    command.arg("-m").arg("512");
    command.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        ovmf.display()
    ));
    command
        .arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
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

    let log = fs::read_to_string(&serial_log)
        .with_context(|| format!("reading {}", serial_log.display()))?;
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
    if log.contains("[PANIC]") || log.contains("panicked at") {
        bail!("smoke boot reached the kernel but then panicked; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Handoff) && !log.contains("boot mode: post-exit-boot-services")
    {
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
    if matches!(mode, LoaderMode::Chainload)
        && !log.contains("standalone boot info present")
    {
        bail!("chainload smoke boot did not enter the standalone kernel; serial output:\n{log}");
    }
    if matches!(mode, LoaderMode::Chainload) && !log.contains("standalone desktop rendered") {
        bail!(
            "chainload smoke boot did not render the standalone desktop; serial output:\n{log}"
        );
    }

    println!("smoke check passed");
    println!("{log}");
    Ok(())
}

fn print_env() -> Result<()> {
    println!("workspace : {}", workspace_root().display());
    println!("qemu      : {}", which("qemu-system-x86_64")?.display());
    println!("ovmf      : {}", discover_ovmf()?.display());
    Ok(())
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
