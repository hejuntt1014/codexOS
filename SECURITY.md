# Security policy

## Support scope

The newest signed codexOS release line receives security corrections. Older release versions remain bootable only when they are not below the device's persisted rollback floor. Development images signed by the automatically generated key under `build/` are never eligible for external distribution.

codexOS is still pre-production. Distribution to end users is prohibited until the remaining platform-security boundaries listed below are closed and independently reviewed.

## Reporting vulnerabilities

Report vulnerabilities through the repository host's private security-advisory channel. Do not place exploit details, signing material, personal data, or embargoed findings in a public issue. A deployment organization must configure and test that private channel before shipping a release.

Response targets start when a reproducible private report is received:

| Severity | Initial triage | Containment decision | Signed correction target |
| --- | ---: | ---: | ---: |
| Critical | 24 hours | 48 hours | 72 hours |
| High | 3 days | 7 days | 14 days |
| Medium | 7 days | 14 days | 30 days |
| Low | 14 days | 30 days | Next scheduled release |

## Release integrity controls

- Every kernel payload is covered by SHA-256 and an Ed25519 signature over a canonical, versioned manifest.
- The UEFI loader embeds a bootstrap public key, honors a CRC-protected persisted trust-root state after a signed transition, and refuses missing, malformed, altered, wrongly keyed, or incorrectly sized payloads.
- Nonvolatile UEFI variables record the highest accepted release version and the active release trust root.
- Two checksummed boot-state records select an A/B system slot. A damaged active slot is rejected before execution and the independently signed fallback is tried; successful fallback selection is written to the alternate state record and reread before execution so the next boot uses the healthy slot directly. If both boot-state records are damaged, the loader scans signed A/B slots, selects the highest verified release allowed by the persisted release floor, then rebuilds and rereads both records before execution.
- GPT release images contain a protective MBR, primary and backup GPT headers, a CRC-verified EFI System Partition entry, and the signed A/B boot set inside that ESP.
- Offline updates auto-detect whole-disk FAT and GPT/ESP installed images, write and verify the inactive slot before switching boot state, then synchronize and verify the redundant slot.
- Production image creation requires an explicit external 32-byte signing key and release version. A transition release may carry the next 32-byte Ed25519 public key through `CODEXOS_NEXT_TRUST_PUBLIC_KEY` or `CODEXOS_NEXT_TRUST_PUBLIC_KEY_HEX`.
- Automated security boots prove kernel-tamper rejection, signature-tamper rejection, rollback rejection, signed trust-root transition, old-root rejection after transition, GPT/ESP boot, A/B recovery, damaged boot-state recovery, installation readback, and updated-slot recovery for both whole-disk FAT and GPT/ESP installed images.

## Key handling

Production signing keys must be created and stored on an offline release workstation with full-disk encryption, restricted operator access, audited backups, and a documented destruction process. The key file must never enter source control, build artifacts, logs, support bundles, or general CI runners. Two-person review is required before each production signing operation.

If signing material is suspected to be exposed, stop release publication immediately, preserve audit evidence, revoke distribution credentials, prepare a signed trust-root transition from a still-trusted release key when possible, and publish the follow-up release with the replacement key only after the transition has booted.

## Open production boundaries

Firmware authentication of `BOOTX64.EFI` through UEFI Secure Boot is not yet enrolled, firmware variable updates are not yet protected by platform-authenticated policy, and signing uses a protected external file rather than a hardware-backed signing service. Physical-machine driver coverage, sandboxed services, memory-safe user applications, measured boot, fuzzing coverage, and independent penetration testing are also incomplete. These gaps prevent a production-security claim even though kernel updates themselves are cryptographically verified.
