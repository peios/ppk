//! The security metadata a package gives one of its files or directories:
//! a security descriptor to replace the one it would otherwise have, and a
//! file's PIP signature (PSPK chapter 3), which the kernel reads from the
//! file's `security.peios.sig` extended attribute.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use peios_uapi as uapi;

/// The suffix that names a file's detached signature beside it in a
/// directory being packed: `<file>.peios.sig`, as PSPU 5.16 and PSPK 3.4
/// name it, and as a signer writes it.
pub(crate) const SIGNATURE_SUFFIX: &str = ".peios.sig";

/// The size of a signature: a version byte and an ML-DSA-65 signature
/// (PSPK 3.2, the kernel's `PKM_KACS_SIGNING_BLOB_LEN`).
pub(crate) const SIGNATURE_BYTES: usize = 3310;

/// The only signature version (PSPK 3.2, `PKM_KACS_SIGNING_VERSION`).
const SIGNATURE_VERSION: u8 = 0x01;

/// The four bytes every ELF file starts with.
const ELF_MAGIC: &[u8] = b"\x7fELF";

/// Every self-relative security descriptor has revision 1 (`sd.h`).
const SD_REVISION: u8 = 1;

/// A SID has revision 1 (MS-DTYP 2.4.2.2).
const SID_REVISION: u8 = 1;

/// A SID has at most 15 subauthorities (MS-DTYP 2.4.2.2).
const SID_MAX_SUBAUTHORITIES: usize = 15;

/// A SID's size before its subauthorities: revision, count and authority.
const SID_FIXED_BYTES: usize = 8;

/// An ACL's revision: 2 for the basic ACE types, 4 with object ACEs
/// (MS-DTYP 2.4.5).
const ACL_REVISIONS: [u8; 2] = [2, 4];

/// Size of an ACL header (`sd.h`).
const ACL_HEADER_BYTES: usize = 8;

/// Size of an ACE header: type, flags and size (MS-DTYP 2.4.4.1).
const ACE_HEADER_BYTES: usize = 4;

/// A self-relative security descriptor that replaces the one a file or
/// directory would otherwise get.
///
/// It is written as lowercase hexadecimal of its bytes, which are kept
/// exactly as given: two layouts of one descriptor are two descriptors, as
/// two spellings of one version are two versions. It must have an owner, a
/// group and a DACL, so that it is the whole descriptor and can be stored
/// as `security.peios.sd` as it is, and its parts must lie within it. Only
/// the kernel judges what it grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityDescriptor {
    bytes: Vec<u8>,
}

impl SecurityDescriptor {
    /// The descriptor `bytes` hold, checked as the type describes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        check_descriptor(&bytes)?;
        Ok(Self { bytes })
    }

    /// The descriptor's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The parts of the descriptor that setting it sets, as `kacs_set_sd`'s
    /// `security_info`: its owner, group and DACL, and its SACL if it has
    /// one.
    pub(crate) fn security_info(&self) -> u32 {
        let mut info =
            uapi::KACS_SECINFO_OWNER | uapi::KACS_SECINFO_GROUP | uapi::KACS_SECINFO_DACL;
        if control(&self.bytes) & uapi::KACS_SD_SACL_PRESENT as u16 != 0 {
            info |= uapi::KACS_SECINFO_SACL;
        }
        info
    }
}

impl FromStr for SecurityDescriptor {
    type Err = anyhow::Error;

    /// The descriptor written in hexadecimal, of either case.
    fn from_str(s: &str) -> Result<Self> {
        if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("not a security descriptor in hexadecimal");
        }
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("checked as hex digits"))
            .collect();
        Self::from_bytes(bytes)
    }
}

impl fmt::Display for SecurityDescriptor {
    /// The descriptor in lowercase hexadecimal, as a manifest writes it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.bytes {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn control(sd: &[u8]) -> u16 {
    u16::from_le_bytes([sd[2], sd[3]])
}

fn u16_at(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
}

/// Check `sd`'s structure: revision 1, self-relative, an owner, a group and
/// a DACL, a SACL exactly when its flag says so, and every part within the
/// descriptor.
fn check_descriptor(sd: &[u8]) -> Result<()> {
    let header = uapi::KACS_SD_HEADER_BYTES as usize;
    if sd.len() < header {
        bail!(
            "security descriptor is {} bytes, shorter than its header",
            sd.len()
        );
    }
    if sd[0] != SD_REVISION {
        bail!(
            "security descriptor revision is {}, not {SD_REVISION}",
            sd[0]
        );
    }
    let control = control(sd);
    if control & uapi::KACS_SD_SELF_RELATIVE as u16 == 0 {
        bail!("security descriptor is not self-relative");
    }
    let [owner, group, sacl, dacl] = [4, 8, 12, 16].map(|at| u32_at(sd, at));
    let sacl_present = control & uapi::KACS_SD_SACL_PRESENT as u16 != 0;
    let dacl_present = control & uapi::KACS_SD_DACL_PRESENT as u16 != 0;
    if owner == 0 || group == 0 {
        bail!("security descriptor has no owner or no group");
    }
    if !dacl_present || dacl == 0 {
        bail!("security descriptor has no DACL");
    }
    if sacl_present != (sacl != 0) {
        bail!("security descriptor's SACL flag and offset disagree");
    }
    check_sid(sd, owner, header).map_err(|err| err.context("owner"))?;
    check_sid(sd, group, header).map_err(|err| err.context("group"))?;
    check_acl(sd, dacl, header).map_err(|err| err.context("DACL"))?;
    if sacl_present {
        check_acl(sd, sacl, header).map_err(|err| err.context("SACL"))?;
    }
    Ok(())
}

fn check_sid(sd: &[u8], at: usize, header: usize) -> Result<()> {
    if at < header || at + SID_FIXED_BYTES > sd.len() {
        bail!("SID at offset {at} is outside the security descriptor");
    }
    let count = usize::from(sd[at + 1]);
    if sd[at] != SID_REVISION || count > SID_MAX_SUBAUTHORITIES {
        bail!("SID at offset {at} is malformed");
    }
    if at + SID_FIXED_BYTES + 4 * count > sd.len() {
        bail!("SID at offset {at} is outside the security descriptor");
    }
    Ok(())
}

fn check_acl(sd: &[u8], at: usize, header: usize) -> Result<()> {
    if at < header || at + ACL_HEADER_BYTES > sd.len() {
        bail!("ACL at offset {at} is outside the security descriptor");
    }
    let size = u16_at(sd, at + 2);
    let count = u16_at(sd, at + 4);
    if !ACL_REVISIONS.contains(&sd[at]) || size < ACL_HEADER_BYTES {
        bail!("ACL at offset {at} is malformed");
    }
    if at + size > sd.len() {
        bail!("ACL at offset {at} is outside the security descriptor");
    }
    let end = at + size;
    let mut ace = at + ACL_HEADER_BYTES;
    for i in 0..count {
        if ace + ACE_HEADER_BYTES > end {
            bail!("ACE {i} of the ACL at offset {at} is outside it");
        }
        let ace_size = u16_at(sd, ace + 2);
        if ace_size < ACE_HEADER_BYTES || ace + ace_size > end {
            bail!("ACE {i} of the ACL at offset {at} is malformed");
        }
        ace += ace_size;
    }
    Ok(())
}

/// Whether `path`, in a directory being packed, is a detached signature:
/// it ends in [`SIGNATURE_SUFFIX`] after a name of its own. A file named
/// just `.peios.sig` is not one.
pub(crate) fn is_signature_path(path: &str) -> bool {
    path.strip_suffix(SIGNATURE_SUFFIX)
        .is_some_and(|target| !target.is_empty() && !target.ends_with('/'))
}

/// Check that `signature` has a signature's shape (PSPU 5.16): exactly
/// [`SIGNATURE_BYTES`] bytes, starting with the version byte. What it
/// signs is for the kernel to verify.
pub(crate) fn check_signature(signature: &[u8]) -> Result<()> {
    if signature.len() != SIGNATURE_BYTES {
        bail!(
            "a signature is {SIGNATURE_BYTES} bytes, but this is {}",
            signature.len()
        );
    }
    if signature[0] != SIGNATURE_VERSION {
        bail!(
            "signature version is {:#04x}, not {SIGNATURE_VERSION:#04x}",
            signature[0]
        );
    }
    Ok(())
}

/// Whether `head`, the start of a file, is ELF's magic. An ELF file carries
/// its signature in its `.peios.sig` section instead (PSPK 3.2).
pub(crate) fn is_elf(head: &[u8]) -> bool {
    head.starts_with(ELF_MAGIC)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// eventd's override of `var/state/eventd` from the old repository: an
    /// owner, a group and a protected DACL of three ACEs.
    pub(crate) const EVENTD: &str = "010004901400000020000000000000002c000000010100000000000512000000\
        01010000000000051200000002005c0003000000000314000000001001010000\
        0000000512000000000318000000001001020000000000052000000020020000\
        0003280000000010010600000000000550000000d2840e756d1f666dfc67a263\
        8a84d887a4a3e576";

    /// gxwi's override of `usr/bin/gxwi-server` from the old repository,
    /// with an unprotected DACL of four ACEs.
    pub(crate) const GXWI_SERVER: &str = "010004801400000020000000000000002c000000010100000000000512000000\
        01010000000000051200000002007c0004000000000014000000001001010000\
        0000000512000000000018000000001001020000000000052000000020020000\
        00001400000000a001010000000000010000000000003400000000a001090000\
        0000000f03000000f00fd709e0ee21c068cb5e4af60ebf884e359bda5e43af72\
        481c2cfb367febf7";

    #[test]
    fn a_real_descriptor_reads_and_writes_back() {
        let sd: SecurityDescriptor = EVENTD.parse().unwrap();
        assert_eq!(sd.to_string(), EVENTD);
        assert_eq!(
            sd.security_info(),
            uapi::KACS_SECINFO_OWNER | uapi::KACS_SECINFO_GROUP | uapi::KACS_SECINFO_DACL
        );
    }

    #[test]
    fn hex_of_either_case_reads_and_writes_as_lowercase() {
        let sd: SecurityDescriptor = EVENTD.to_uppercase().parse().unwrap();
        assert_eq!(sd.to_string(), EVENTD);
    }

    /// A change to a descriptor's bytes.
    type Change = Box<dyn Fn(&mut Vec<u8>)>;

    #[test]
    fn a_descriptor_that_is_not_whole_or_not_within_itself_is_refused() {
        let good: SecurityDescriptor = EVENTD.parse().unwrap();
        let cases: [(&str, Change); 7] = [
            ("shorter than its header", Box::new(|sd| sd.truncate(10))),
            ("revision", Box::new(|sd| sd[0] = 2)),
            ("not self-relative", Box::new(|sd| sd[3] &= 0x7f)),
            ("no owner", Box::new(|sd| sd[4..8].fill(0))),
            ("no DACL", Box::new(|sd| sd[2] &= !0x04)),
            ("SACL flag", Box::new(|sd| sd[2] |= 0x10)),
            ("outside", Box::new(|sd| sd.truncate(sd.len() - 1))),
        ];
        for (expected, change) in cases {
            let mut bytes = good.as_bytes().to_vec();
            change(&mut bytes);
            let err = SecurityDescriptor::from_bytes(bytes).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{expected}: {err:#}");
        }
    }

    #[test]
    fn odd_or_non_hex_text_is_not_a_descriptor() {
        for text in ["", "0", "zz", &format!("{EVENTD}0")] {
            assert!(text.parse::<SecurityDescriptor>().is_err(), "{text}");
        }
    }

    #[test]
    fn a_signature_path_has_a_name_before_its_suffix() {
        assert!(is_signature_path("lib/firmware/a.bin.peios.sig"));
        assert!(is_signature_path("a.peios.sig"));
        assert!(!is_signature_path(".peios.sig"));
        assert!(!is_signature_path("lib/.peios.sig"));
        assert!(!is_signature_path("lib/a.bin"));
    }

    #[test]
    fn a_signature_is_exactly_its_size_and_version() {
        let mut signature = vec![0u8; SIGNATURE_BYTES];
        signature[0] = SIGNATURE_VERSION;
        check_signature(&signature).unwrap();
        signature[0] = 2;
        assert!(check_signature(&signature).is_err());
        assert!(check_signature(&[SIGNATURE_VERSION; SIGNATURE_BYTES - 1]).is_err());
    }
}
