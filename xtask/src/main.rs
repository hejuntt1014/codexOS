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
const LOADER_BIN: &str = "uefi-loader.efi";
const IMAGE_SIZE_BYTES: u64 = 64 * 1024 * 1024;

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("build") => {
            build_loader(false)?;
        }
        Some("image") => {
            let loader = build_loader(false)?;
            let image = make_image(&loader)?;
            println!("disk image ready: {}", image.display());
        }
        Some("run") => {
            let loader = build_loader(false)?;
            let image = make_image(&loader)?;
            run_qemu(&image, false)?;
        }
        Some("debug") => {
            let loader = build_loader(false)?;
            let image = make_image(&loader)?;
            run_qemu(&image, true)?;
        }
        Some("smoke") => {
            let loader = build_loader(false)?;
            let image = make_image(&loader)?;
            smoke_qemu(&image)?;
        }
        Some("env") => print_env()?,
        _ => print_help(),
    }

    Ok(())
}

fn print_help() {
    println!("codexOS xtask");
    println!("  cargo xtask build   - build the UEFI image");
    println!("  cargo xtask image   - build and pack a FAT disk image");
    println!("  cargo xtask run     - launch QEMU with serial logs on stdio");
    println!("  cargo xtask debug   - launch QEMU paused with a gdb stub on :1234");
    println!("  cargo xtask smoke   - boot headlessly and verify the kernel log");
    println!("  cargo xtask env     - print discovered tool paths");
}

fn build_loader(release: bool) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command.arg("build");
    command.arg("-p").arg("uefi-loader");
    command.arg("--target").arg(EFI_TARGET);
    if release {
        command.arg("--release");
    }

    run(command, "building the UEFI loader")?;

    let profile = if release { "release" } else { "debug" };
    let loader = workspace_root()
        .join("target")
        .join(EFI_TARGET)
        .join(profile)
        .join(LOADER_BIN);

    if !loader.exists() {
        bail!("expected loader binary at {}", loader.display());
    }

    Ok(loader)
}

fn make_image(loader: &Path) -> Result<PathBuf> {
    let build_dir = workspace_root().join("build");
    fs::create_dir_all(&build_dir)?;

    let image_path = build_dir.join("codexos.img");
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
    copy_loader_into_image(image, loader)?;

    Ok(image_path)
}

fn format_disk(mut image: File) -> Result<()> {
    image.seek(SeekFrom::Start(0))?;
    fatfs::format_volume(&mut image, FormatVolumeOptions::new())
        .context("formatting FAT volume")?;
    Ok(())
}

fn copy_loader_into_image(image: File, loader: &Path) -> Result<()> {
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

fn smoke_qemu(image: &Path) -> Result<()> {
    let qemu = env::var("CODEXOS_QEMU")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("qemu-system-x86_64"));
    let ovmf = discover_ovmf()?;
    let serial_log = workspace_root().join("build").join("serial.log");
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
    if !log.contains("codexOS kernel entered") {
        bail!("smoke boot did not reach the kernel; serial output:\n{log}");
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
