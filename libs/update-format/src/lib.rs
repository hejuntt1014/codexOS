#![no_std]

pub const MANIFEST_MAGIC: [u8; 8] = *b"CDXUPD1\0";
pub const LEGACY_MANIFEST_FORMAT_VERSION: u32 = 1;
pub const MANIFEST_FORMAT_VERSION: u32 = 2;
pub const LEGACY_MANIFEST_BYTES: usize = 160;
pub const MANIFEST_BYTES: usize = 224;
pub const LEGACY_SIGNING_BYTES: usize = 80;
pub const SIGNING_BYTES: usize = 160;
pub const SHA256_BYTES: usize = 32;
pub const PUBLIC_KEY_BYTES: usize = 32;
pub const KEY_ID_BYTES: usize = 16;
pub const ED25519_SIGNATURE_BYTES: usize = 64;
pub const BOOT_STATE_MAGIC: [u8; 8] = *b"CDXBOOT1";
pub const BOOT_STATE_FORMAT_VERSION: u32 = 1;
pub const BOOT_STATE_BYTES: usize = 64;
pub const TRUST_ROOT_STATE_MAGIC: [u8; 8] = *b"CDXTRST1";
pub const TRUST_ROOT_STATE_FORMAT_VERSION: u32 = 1;
pub const TRUST_ROOT_STATE_BYTES: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    InvalidLength,
    InvalidMagic,
    UnsupportedFormat,
    InvalidHeaderSize,
    InvalidReleaseVersion,
    InvalidKernelSize,
    InvalidKeyId,
    InvalidNextTrustKey,
    InvalidSignature,
    NonzeroReservedBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootStateError {
    InvalidLength,
    InvalidMagic,
    UnsupportedFormat,
    InvalidHeaderSize,
    InvalidGeneration,
    InvalidSlot,
    InvalidReleaseVersion,
    InvalidChecksum,
    NonzeroReservedBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustRootStateError {
    InvalidLength,
    InvalidMagic,
    UnsupportedFormat,
    InvalidHeaderSize,
    InvalidActivationRelease,
    InvalidPublicKey,
    InvalidKeyId,
    InvalidChecksum,
    NonzeroReservedBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemSlot {
    A,
    B,
}

impl SystemSlot {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::A),
            1 => Some(Self::B),
            _ => None,
        }
    }

    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootState {
    pub generation: u64,
    pub active_slot: SystemSlot,
    pub active_release_version: u64,
}

impl BootState {
    pub fn encode(self) -> [u8; BOOT_STATE_BYTES] {
        let mut output = [0_u8; BOOT_STATE_BYTES];
        output[0..8].copy_from_slice(&BOOT_STATE_MAGIC);
        put_u32(&mut output, 8, BOOT_STATE_FORMAT_VERSION);
        put_u32(&mut output, 12, BOOT_STATE_BYTES as u32);
        put_u64(&mut output, 16, self.generation);
        output[24] = self.active_slot.as_u8();
        put_u64(&mut output, 32, self.active_release_version);
        let checksum = crc32(&output);
        put_u32(&mut output, 40, checksum);
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, BootStateError> {
        if input.len() != BOOT_STATE_BYTES {
            return Err(BootStateError::InvalidLength);
        }
        if input[0..8] != BOOT_STATE_MAGIC {
            return Err(BootStateError::InvalidMagic);
        }
        if get_u32(input, 8) != BOOT_STATE_FORMAT_VERSION {
            return Err(BootStateError::UnsupportedFormat);
        }
        if get_u32(input, 12) as usize != BOOT_STATE_BYTES {
            return Err(BootStateError::InvalidHeaderSize);
        }
        let generation = get_u64(input, 16);
        if generation == 0 {
            return Err(BootStateError::InvalidGeneration);
        }
        let active_slot = SystemSlot::from_u8(input[24]).ok_or(BootStateError::InvalidSlot)?;
        let active_release_version = get_u64(input, 32);
        if active_release_version == 0 {
            return Err(BootStateError::InvalidReleaseVersion);
        }
        if input[25..32].iter().any(|byte| *byte != 0) || input[44..].iter().any(|byte| *byte != 0)
        {
            return Err(BootStateError::NonzeroReservedBytes);
        }
        let stored_checksum = get_u32(input, 40);
        let mut copy = [0_u8; BOOT_STATE_BYTES];
        copy.copy_from_slice(input);
        put_u32(&mut copy, 40, 0);
        if crc32(&copy) != stored_checksum {
            return Err(BootStateError::InvalidChecksum);
        }
        Ok(Self {
            generation,
            active_slot,
            active_release_version,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateManifest {
    pub format_version: u32,
    pub release_version: u64,
    pub kernel_size: u64,
    pub kernel_sha256: [u8; SHA256_BYTES],
    pub key_id: [u8; KEY_ID_BYTES],
    pub next_trust_public_key: [u8; PUBLIC_KEY_BYTES],
    pub next_trust_key_id: [u8; KEY_ID_BYTES],
    pub signature: [u8; ED25519_SIGNATURE_BYTES],
}

impl UpdateManifest {
    pub const fn unsigned(
        release_version: u64,
        kernel_size: u64,
        kernel_sha256: [u8; SHA256_BYTES],
        key_id: [u8; KEY_ID_BYTES],
    ) -> Self {
        Self {
            format_version: MANIFEST_FORMAT_VERSION,
            release_version,
            kernel_size,
            kernel_sha256,
            key_id,
            next_trust_public_key: [0; PUBLIC_KEY_BYTES],
            next_trust_key_id: [0; KEY_ID_BYTES],
            signature: [0; ED25519_SIGNATURE_BYTES],
        }
    }

    pub const fn unsigned_with_next_trust_key(
        release_version: u64,
        kernel_size: u64,
        kernel_sha256: [u8; SHA256_BYTES],
        key_id: [u8; KEY_ID_BYTES],
        next_trust_public_key: [u8; PUBLIC_KEY_BYTES],
        next_trust_key_id: [u8; KEY_ID_BYTES],
    ) -> Self {
        Self {
            format_version: MANIFEST_FORMAT_VERSION,
            release_version,
            kernel_size,
            kernel_sha256,
            key_id,
            next_trust_public_key,
            next_trust_key_id,
            signature: [0; ED25519_SIGNATURE_BYTES],
        }
    }

    pub fn has_next_trust_key(&self) -> bool {
        self.next_trust_public_key != [0; PUBLIC_KEY_BYTES]
    }

    pub const fn signing_len(&self) -> usize {
        match self.format_version {
            LEGACY_MANIFEST_FORMAT_VERSION => LEGACY_SIGNING_BYTES,
            _ => SIGNING_BYTES,
        }
    }

    pub fn signing_bytes(&self) -> [u8; SIGNING_BYTES] {
        let mut signing = [0_u8; SIGNING_BYTES];
        signing[0..8].copy_from_slice(&MANIFEST_MAGIC);
        match self.format_version {
            LEGACY_MANIFEST_FORMAT_VERSION => {
                put_u32(&mut signing, 8, LEGACY_MANIFEST_FORMAT_VERSION);
                put_u32(&mut signing, 12, LEGACY_MANIFEST_BYTES as u32);
                put_u64(&mut signing, 16, self.release_version);
                put_u64(&mut signing, 24, self.kernel_size);
                signing[32..64].copy_from_slice(&self.kernel_sha256);
                signing[64..80].copy_from_slice(&self.key_id);
            }
            _ => {
                put_u32(&mut signing, 8, MANIFEST_FORMAT_VERSION);
                put_u32(&mut signing, 12, MANIFEST_BYTES as u32);
                put_u64(&mut signing, 16, self.release_version);
                put_u64(&mut signing, 24, self.kernel_size);
                signing[32..64].copy_from_slice(&self.kernel_sha256);
                signing[64..80].copy_from_slice(&self.key_id);
                signing[80..112].copy_from_slice(&self.next_trust_public_key);
                signing[112..128].copy_from_slice(&self.next_trust_key_id);
            }
        }
        signing
    }

    pub fn encode(&self) -> [u8; MANIFEST_BYTES] {
        let mut output = [0_u8; MANIFEST_BYTES];
        output[..SIGNING_BYTES].copy_from_slice(&self.signing_bytes());
        output[160..224].copy_from_slice(&self.signature);
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, ManifestError> {
        if !matches!(input.len(), LEGACY_MANIFEST_BYTES | MANIFEST_BYTES) {
            return Err(ManifestError::InvalidLength);
        }
        if input[0..8] != MANIFEST_MAGIC {
            return Err(ManifestError::InvalidMagic);
        }
        let format_version = get_u32(input, 8);
        let header_size = get_u32(input, 12) as usize;
        let release_version = get_u64(input, 16);
        if release_version == 0 {
            return Err(ManifestError::InvalidReleaseVersion);
        }
        let kernel_size = get_u64(input, 24);
        if kernel_size == 0 {
            return Err(ManifestError::InvalidKernelSize);
        }
        let mut kernel_sha256 = [0_u8; SHA256_BYTES];
        kernel_sha256.copy_from_slice(&input[32..64]);
        let mut key_id = [0_u8; KEY_ID_BYTES];
        key_id.copy_from_slice(&input[64..80]);
        if key_id == [0; KEY_ID_BYTES] {
            return Err(ManifestError::InvalidKeyId);
        }
        let mut signature = [0_u8; ED25519_SIGNATURE_BYTES];
        let mut next_trust_public_key = [0_u8; PUBLIC_KEY_BYTES];
        let mut next_trust_key_id = [0_u8; KEY_ID_BYTES];
        match format_version {
            LEGACY_MANIFEST_FORMAT_VERSION => {
                if input.len() != LEGACY_MANIFEST_BYTES || header_size != LEGACY_MANIFEST_BYTES {
                    return Err(ManifestError::InvalidHeaderSize);
                }
                signature.copy_from_slice(&input[80..144]);
                if input[144..160].iter().any(|byte| *byte != 0) {
                    return Err(ManifestError::NonzeroReservedBytes);
                }
            }
            MANIFEST_FORMAT_VERSION => {
                if input.len() != MANIFEST_BYTES || header_size != MANIFEST_BYTES {
                    return Err(ManifestError::InvalidHeaderSize);
                }
                next_trust_public_key.copy_from_slice(&input[80..112]);
                next_trust_key_id.copy_from_slice(&input[112..128]);
                let next_key_is_zero = next_trust_public_key == [0; PUBLIC_KEY_BYTES];
                let next_id_is_zero = next_trust_key_id == [0; KEY_ID_BYTES];
                if next_key_is_zero != next_id_is_zero {
                    return Err(ManifestError::InvalidNextTrustKey);
                }
                if input[128..160].iter().any(|byte| *byte != 0) {
                    return Err(ManifestError::NonzeroReservedBytes);
                }
                signature.copy_from_slice(&input[160..224]);
            }
            _ => return Err(ManifestError::UnsupportedFormat),
        }
        if signature == [0; ED25519_SIGNATURE_BYTES] {
            return Err(ManifestError::InvalidSignature);
        }
        Ok(Self {
            format_version,
            release_version,
            kernel_size,
            kernel_sha256,
            key_id,
            next_trust_public_key,
            next_trust_key_id,
            signature,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustRootState {
    pub activation_release_version: u64,
    pub public_key: [u8; PUBLIC_KEY_BYTES],
    pub key_id: [u8; KEY_ID_BYTES],
}

impl TrustRootState {
    pub fn encode(self) -> [u8; TRUST_ROOT_STATE_BYTES] {
        let mut output = [0_u8; TRUST_ROOT_STATE_BYTES];
        output[0..8].copy_from_slice(&TRUST_ROOT_STATE_MAGIC);
        put_u32(&mut output, 8, TRUST_ROOT_STATE_FORMAT_VERSION);
        put_u32(&mut output, 12, TRUST_ROOT_STATE_BYTES as u32);
        put_u64(&mut output, 16, self.activation_release_version);
        output[24..56].copy_from_slice(&self.public_key);
        output[56..72].copy_from_slice(&self.key_id);
        let checksum = crc32(&output);
        put_u32(&mut output, 72, checksum);
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, TrustRootStateError> {
        if input.len() != TRUST_ROOT_STATE_BYTES {
            return Err(TrustRootStateError::InvalidLength);
        }
        if input[0..8] != TRUST_ROOT_STATE_MAGIC {
            return Err(TrustRootStateError::InvalidMagic);
        }
        if get_u32(input, 8) != TRUST_ROOT_STATE_FORMAT_VERSION {
            return Err(TrustRootStateError::UnsupportedFormat);
        }
        if get_u32(input, 12) as usize != TRUST_ROOT_STATE_BYTES {
            return Err(TrustRootStateError::InvalidHeaderSize);
        }
        let activation_release_version = get_u64(input, 16);
        if activation_release_version == 0 {
            return Err(TrustRootStateError::InvalidActivationRelease);
        }
        let mut public_key = [0_u8; PUBLIC_KEY_BYTES];
        public_key.copy_from_slice(&input[24..56]);
        if public_key == [0; PUBLIC_KEY_BYTES] {
            return Err(TrustRootStateError::InvalidPublicKey);
        }
        let mut key_id = [0_u8; KEY_ID_BYTES];
        key_id.copy_from_slice(&input[56..72]);
        if key_id == [0; KEY_ID_BYTES] {
            return Err(TrustRootStateError::InvalidKeyId);
        }
        if input[76..80].iter().any(|byte| *byte != 0) {
            return Err(TrustRootStateError::NonzeroReservedBytes);
        }
        let stored_checksum = get_u32(input, 72);
        let mut copy = [0_u8; TRUST_ROOT_STATE_BYTES];
        copy.copy_from_slice(input);
        put_u32(&mut copy, 72, 0);
        if crc32(&copy) != stored_checksum {
            return Err(TrustRootStateError::InvalidChecksum);
        }
        Ok(Self {
            activation_release_version,
            public_key,
            key_id,
        })
    }
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
}

fn get_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
}

fn crc32(bytes: &[u8]) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> UpdateManifest {
        let mut manifest =
            UpdateManifest::unsigned(7, 123_456, [0x5a; SHA256_BYTES], [0xa5; KEY_ID_BYTES]);
        manifest.signature = [0x3c; ED25519_SIGNATURE_BYTES];
        manifest
    }

    #[test]
    fn manifest_round_trip_is_canonical() {
        let manifest = valid_manifest();
        let encoded = manifest.encode();
        assert_eq!(encoded.len(), MANIFEST_BYTES);
        assert_eq!(UpdateManifest::decode(&encoded), Ok(manifest));
        assert_eq!(manifest.signing_len(), SIGNING_BYTES);
        assert_eq!(manifest.signing_bytes(), encoded[..SIGNING_BYTES]);
    }

    #[test]
    fn manifest_signs_next_trust_root() {
        let mut manifest = UpdateManifest::unsigned_with_next_trust_key(
            8,
            65_536,
            [0x11; SHA256_BYTES],
            [0x22; KEY_ID_BYTES],
            [0x33; PUBLIC_KEY_BYTES],
            [0x44; KEY_ID_BYTES],
        );
        manifest.signature = [0x55; ED25519_SIGNATURE_BYTES];
        let encoded = manifest.encode();
        assert_eq!(encoded[80..112], [0x33; PUBLIC_KEY_BYTES]);
        assert_eq!(encoded[112..128], [0x44; KEY_ID_BYTES]);
        assert_eq!(encoded[160..224], [0x55; ED25519_SIGNATURE_BYTES]);
        assert_eq!(UpdateManifest::decode(&encoded), Ok(manifest));
        assert!(manifest.has_next_trust_key());
    }

    #[test]
    fn legacy_manifest_decode_preserves_original_signing_span() {
        let mut encoded = [0_u8; LEGACY_MANIFEST_BYTES];
        encoded[0..8].copy_from_slice(&MANIFEST_MAGIC);
        put_u32(&mut encoded, 8, LEGACY_MANIFEST_FORMAT_VERSION);
        put_u32(&mut encoded, 12, LEGACY_MANIFEST_BYTES as u32);
        put_u64(&mut encoded, 16, 3);
        put_u64(&mut encoded, 24, 4096);
        encoded[32..64].fill(0x21);
        encoded[64..80].fill(0x43);
        encoded[80..144].fill(0x65);

        let manifest = UpdateManifest::decode(&encoded).unwrap();
        assert_eq!(manifest.format_version, LEGACY_MANIFEST_FORMAT_VERSION);
        assert_eq!(manifest.signing_len(), LEGACY_SIGNING_BYTES);
        assert_eq!(
            &manifest.signing_bytes()[..LEGACY_SIGNING_BYTES],
            &encoded[..LEGACY_SIGNING_BYTES]
        );
        assert!(!manifest.has_next_trust_key());
    }

    #[test]
    fn rejects_unsigned_or_extended_manifests() {
        let mut unsigned = valid_manifest().encode();
        unsigned[160..224].fill(0);
        assert_eq!(
            UpdateManifest::decode(&unsigned),
            Err(ManifestError::InvalidSignature)
        );
        let mut extended = valid_manifest().encode();
        extended[159] = 1;
        assert_eq!(
            UpdateManifest::decode(&extended),
            Err(ManifestError::NonzeroReservedBytes)
        );
    }

    #[test]
    fn rejects_partial_next_trust_root() {
        let mut encoded = valid_manifest().encode();
        encoded[80] = 1;
        assert_eq!(
            UpdateManifest::decode(&encoded),
            Err(ManifestError::InvalidNextTrustKey)
        );
    }

    #[test]
    fn boot_state_round_trip_and_slot_switch_are_canonical() {
        let state = BootState {
            generation: 42,
            active_slot: SystemSlot::B,
            active_release_version: 9,
        };
        let encoded = state.encode();
        assert_eq!(BootState::decode(&encoded), Ok(state));
        assert_eq!(state.active_slot.other(), SystemSlot::A);
    }

    #[test]
    fn boot_state_checksum_detects_active_slot_corruption() {
        let mut encoded = BootState {
            generation: 1,
            active_slot: SystemSlot::A,
            active_release_version: 1,
        }
        .encode();
        encoded[24] = 1;
        assert_eq!(
            BootState::decode(&encoded),
            Err(BootStateError::InvalidChecksum)
        );
    }

    #[test]
    fn trust_root_state_round_trip_is_checksummed() {
        let state = TrustRootState {
            activation_release_version: 12,
            public_key: [0x7b; PUBLIC_KEY_BYTES],
            key_id: [0xb7; KEY_ID_BYTES],
        };
        let encoded = state.encode();
        assert_eq!(TrustRootState::decode(&encoded), Ok(state));

        let mut damaged = encoded;
        damaged[24] ^= 1;
        assert_eq!(
            TrustRootState::decode(&damaged),
            Err(TrustRootStateError::InvalidChecksum)
        );
    }
}
