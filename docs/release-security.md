# Signed release and recovery operations

## Development verification

The development workflow creates a random Ed25519 key at `build/codexos-development-signing-key.bin`. The path is ignored by Git. Images signed by this key are local engineering artifacts.

```powershell
cargo xtask smoke-security
cargo xtask smoke-trust-rotation
cargo xtask smoke-gpt-esp
cargo xtask smoke-recovery
cargo xtask smoke-bootstate-recovery
cargo xtask smoke-install
cargo xtask smoke-gpt-install
```

The gates respectively prove signature and rollback rejection, signed trust-root transition with old-root rejection, GPT/EFI System Partition boot, fallback-slot boot, damaged boot-state recovery through signed slot scanning, the whole-disk FAT install/update/recovery sequence, and the GPT/ESP install/update/recovery sequence.

## Production release

Provide a raw 32-byte Ed25519 signing seed from the protected release workstation and a strictly increasing integer version:

```powershell
$env:CODEXOS_SIGNING_KEY = 'X:\secured-release\codexos-ed25519.seed'
$env:CODEXOS_RELEASE_VERSION = '42'
cargo xtask release-gpt-image 'X:\release-output\codexos-42.img'
```

The command refuses an existing destination, embeds the corresponding public key in the loader, writes a protective MBR, primary and backup GPT structures, a FAT EFI System Partition, both signed system slots, and two CRC-protected boot-state generations, then flushes the image, rereads it, validates the GPT/ESP layout, verifies the signed release files, and compares its SHA-256 digest.

The older `release-image` command still emits a whole-disk FAT engineering image for compatibility with existing checks. Production release candidates should use `release-gpt-image` so firmware sees a standard EFI System Partition.

## Trust-root transition

Create the replacement key on the protected release workstation, then export only its public key for the transition release:

```powershell
cargo xtask derive-public-key 'X:\secured-release\codexos-ed25519-v2.seed' 'X:\secured-release\codexos-ed25519-v2.pub'
```

Sign a higher release with the currently trusted key and attach the replacement public key:

```powershell
$env:CODEXOS_SIGNING_KEY = 'X:\secured-release\codexos-ed25519.seed'
$env:CODEXOS_RELEASE_VERSION = '44'
$env:CODEXOS_NEXT_TRUST_PUBLIC_KEY = 'X:\secured-release\codexos-ed25519-v2.pub'
cargo xtask apply-update 'D:\codexos-installed.img'
```

The manifest signature covers the replacement public key and key identifier. After the release passes signature, hash, and anti-rollback checks, the loader writes `CodexOsTrustRoot` as a nonvolatile UEFI variable, reads it back, and then uses that persisted root for later releases. A later release signed by the previous key is rejected even if its version is higher.

## Offline installation and update

Create and verify an installed A/B image:

```powershell
cargo xtask install-gpt 'D:\codexos-installed.img'
```

`install-gpt` creates a GPT/ESP image, copies it to the requested destination without overwriting an existing file, flushes it, compares a full-image readback SHA-256 digest, and validates the copied GPT/ESP contents. It writes only to the file path supplied by the operator; raw physical-disk writes still require an explicit deployment tool and operator policy outside this repository.

Apply a higher signed version to an installed image:

```powershell
$env:CODEXOS_SIGNING_KEY = 'X:\secured-release\codexos-ed25519.seed'
$env:CODEXOS_RELEASE_VERSION = '43'
cargo xtask apply-update 'D:\codexos-installed.img'
```

The updater writes the inactive slot first, verifies the reread kernel and signature, advances the alternate boot-state record, then synchronizes and verifies the redundant slot. A power loss before the state write leaves the prior slot selected; a power loss after it leaves the already verified new slot selected.

`apply-update` inspects the installed image before writing. Whole-disk FAT images are updated at the disk root, while GPT images are updated inside the validated EFI System Partition and then reread through the GPT/ESP verifier.

## Recovery behavior

At boot, the loader validates the newest usable boot-state record and the selected slot. If its manifest, hash, signature, ELF structure, or staging operation fails, the other signed slot is attempted. If both boot-state records are damaged, the loader scans the signed A/B slots and selects the highest verified release that passes the nonvolatile release floor. Recovery never authorizes an older prohibited release.
