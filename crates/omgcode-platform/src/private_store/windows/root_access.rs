//! Operator-confirmed removal of extra grants from a private root (ADR-0072).
//!
//! The application data root inherits its DACL, so another program can leave
//! a grant on it that the private-root rule refuses. This module lists such
//! grants and, only when the caller passes back the exact list the operator
//! confirmed, removes them. Nothing here runs automatically.
//!
//! The rewrite keeps every entry for a trusted principal and every deny
//! entry, makes inherited entries explicit, and protects the DACL so the
//! parent cannot pass the grant down again. Deny entries are placed first, which
//! only narrows access. The owner is never changed. A root with an untrusted
//! owner or an entry type this module does not decode is not repaired. Nor is
//! one whose new DACL would not give the user every right the store asks for
//! on its root. Two checks decide that. The user's own entries on the folder,
//! less the user's own deny entries, must grant those rights; this does not
//! depend on the token, which may be elevated now and not when the store
//! opens. Windows' own `AccessCheck` against the process token must also grant
//! them, so a kept deny entry for one of the user's groups counts. Children whose DACL is not protected
//! receive the new inheritable entries through the native propagation of
//! `SetSecurityInfo`; the store's own children are protected and unchanged.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AccessCheck, AclSizeInformation, AddAce,
    DACL_SECURITY_INFORMATION, DuplicateToken, GENERIC_MAPPING, GetAce, GetAclInformation,
    INHERIT_ONLY_ACE, INHERITED_ACE, InitializeAcl, InitializeSecurityDescriptor,
    LookupAccountSidW, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_DESCRIPTOR, SID_NAME_USE, SecurityIdentification, SetSecurityDescriptorDacl,
    SetSecurityDescriptorGroup, SetSecurityDescriptorOwner, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ACCESS_RIGHTS, FILE_ALL_ACCESS, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::SystemServices::{MAXDWORD, SECURITY_DESCRIPTOR_REVISION};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{
    Ace, MAX_ROOT_ACES, ObjectSecurity, PRIVATE_DIRECTORY_ACCESS, Principals, Sid, VolumeRole,
    invalid_data, invalid_input, require_supported_filesystem, require_trusted_root,
};
use crate::broker::windows::{
    DIRECTORY_ACCESS, SHARE_READ_WRITE, query_identity, validate_directory,
};
use crate::private_store::{ExtraGrant, RootAccess, RootRepairError};

/// Enough to validate the directory, read its descriptor, and replace its DACL.
const REPAIR_ACCESS: FILE_ACCESS_RIGHTS = DIRECTORY_ACCESS | READ_CONTROL | WRITE_DAC;
/// Enough to validate the directory and read its descriptor.
const INSPECT_ACCESS: FILE_ACCESS_RIGHTS = DIRECTORY_ACCESS | READ_CONTROL;
/// Account and domain names longer than this are shown by SID alone.
const ACCOUNT_UNITS: usize = 256;
/// `INHERITED_ACE` in the one-byte `AceFlags` field.
const INHERITED_FLAG: u8 = 0x10;
const _: () = assert!(INHERITED_ACE == INHERITED_FLAG as u32);
/// Room for the privilege set `AccessCheck` reports. The rights checked here
/// need no privilege, so a few entries are ample.
const PRIVILEGE_BYTES: usize = 256;
/// How Windows maps generic rights on files and folders.
const FILE_MAPPING: GENERIC_MAPPING = GENERIC_MAPPING {
    GenericRead: FILE_GENERIC_READ,
    GenericWrite: FILE_GENERIC_WRITE,
    GenericExecute: FILE_GENERIC_EXECUTE,
    GenericAll: FILE_ALL_ACCESS,
};

const NOT_A_FOLDER: &str = "the folder is a link or is not an ordinary folder";
const UNSUPPORTED_VOLUME: &str = "the folder is not on an NTFS or ReFS volume";
const UNTRUSTED_OWNER: &str = "the folder is owned by another account";
const UNKNOWN_ENTRY: &str = "the folder has a permission entry of a kind OMGCode does not edit";
const NO_USER_ACCESS: &str =
    "removing the extra grants would leave your account without the access OMGCode needs";

/// The grants to remove and the DACL that would replace the current one.
struct Plan {
    /// Untrusted principals with an allow entry, sorted by SID and unique.
    grants: Vec<ExtraGrant>,
    /// The kept entries with the inherited flag cleared, denies then allows,
    /// as an ACL in aligned storage.
    acl: Vec<u64>,
}

/// Lists the extra grants on an existing root, opened without following a
/// final reparse point.
pub(crate) fn inspect(path: &Path) -> io::Result<RootAccess> {
    let file = open(path, INSPECT_ACCESS)?;
    Ok(match plan(&file, &Principals::current()?)? {
        Err(reason) => RootAccess::NotRepairable(reason),
        Ok(plan) if plan.grants.is_empty() => RootAccess::Private,
        Ok(plan) => RootAccess::Repairable(plan.grants),
    })
}

/// Removes the extra grants when they still equal `confirmed`, then rechecks
/// the private-root rule from the same handle.
pub(crate) fn remove(path: &Path, confirmed: &[ExtraGrant]) -> Result<(), RootRepairError> {
    let principals = Principals::current().map_err(RootRepairError::Io)?;
    let file = open(path, REPAIR_ACCESS).map_err(RootRepairError::Io)?;
    let mut plan = plan(&file, &principals)
        .map_err(RootRepairError::Io)?
        .map_err(RootRepairError::NotRepairable)?;
    if !same_principals(&plan.grants, confirmed) {
        return Err(RootRepairError::Changed);
    }
    if plan.grants.is_empty() {
        return Ok(());
    }
    apply(&file, &mut plan.acl).map_err(RootRepairError::Io)?;
    require_trusted_root(&file, &principals).map_err(RootRepairError::Io)
}

fn open(path: &Path, access: FILE_ACCESS_RIGHTS) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(invalid_input("private root must be absolute"));
    }
    OpenOptions::new()
        .access_mode(access)
        .share_mode(SHARE_READ_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Compares by SID only; account names are for display.
fn same_principals(found: &[ExtraGrant], confirmed: &[ExtraGrant]) -> bool {
    let mut expected: Vec<&str> = confirmed.iter().map(|grant| grant.sid.as_str()).collect();
    expected.sort_unstable();
    expected.dedup();
    found.iter().map(|grant| grant.sid.as_str()).eq(expected)
}

/// Decides what a repair would remove and keep, or why it cannot run.
fn plan(file: &File, principals: &Principals) -> io::Result<Result<Plan, &'static str>> {
    let volume = query_identity(file)?.VolumeSerialNumber;
    match validate_directory(file, volume) {
        Err(error) if error.kind() == io::ErrorKind::InvalidData => return Ok(Err(NOT_A_FOLDER)),
        other => other?,
    }
    match require_supported_filesystem(file, VolumeRole::PrivateStore) {
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            return Ok(Err(UNSUPPORTED_VOLUME));
        }
        other => {
            other?;
        }
    }
    let security = ObjectSecurity::read(file)?;
    let owner = security.owner()?;
    if !principals.trusted(&owner) {
        return Ok(Err(UNTRUSTED_OWNER));
    }
    let decoded = security.aces()?;
    let raw = raw_aces(&security)?;
    if decoded.len() != raw.len() {
        return Err(invalid_data("DACL changed while it was read"));
    }
    let mut grants = Vec::new();
    let (mut denies, mut allows) = (Vec::new(), Vec::new());
    // Rights the user's own entries give on the folder itself, less those its
    // own deny entries take away. Inherit-only entries apply only to children.
    let (mut user_allowed, mut user_denied) = (0u32, 0u32);
    for (ace, bytes) in decoded.into_iter().zip(raw) {
        match ace {
            Ace::Allowed { sid, .. } if !principals.trusted(&sid) => grants.push(describe(&sid)?),
            Ace::Allowed { flags, mask, sid } => {
                if flags & INHERIT_ONLY_ACE == 0 && principals.user.equals(&sid) {
                    user_allowed |= mask;
                }
                allows.push(explicit(bytes));
            }
            Ace::Denied { flags, mask, sid } => {
                if flags & INHERIT_ONLY_ACE == 0 && principals.user.equals(&sid) {
                    user_denied |= mask;
                }
                denies.push(explicit(bytes));
            }
            Ace::Other { .. } => return Ok(Err(UNKNOWN_ENTRY)),
        }
    }
    grants.sort();
    grants.dedup_by(|left, right| left.sid == right.sid);
    denies.append(&mut allows);
    let mut acl = build_acl(&denies)?;
    if grants.is_empty() {
        return Ok(Ok(Plan { grants, acl }));
    }
    // Two checks, both required. The user's own entries must grant the
    // store's rights, whatever the token: `AccessCheck` judges this process,
    // which may be elevated or hold privileges an ordinary launch does not.
    // `AccessCheck` then applies what the first check cannot see, such as a
    // kept deny entry for one of the user's groups.
    let own = user_allowed & !user_denied;
    if own & PRIVATE_DIRECTORY_ACCESS != PRIVATE_DIRECTORY_ACCESS
        || !store_could_open(&owner, &mut acl)?
    {
        return Ok(Err(NO_USER_ACCESS));
    }
    Ok(Ok(Plan { grants, acl }))
}

/// Whether this process would get every right the private store asks for on
/// its root if the root carried `acl` and `owner`, as Windows decides it.
#[allow(unsafe_code)]
fn store_could_open(owner: &Sid, acl: &mut [u64]) -> io::Result<bool> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle is always valid and `raw` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut raw) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success returned a new token handle that this value now owns.
    let primary = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: `primary` is a live token opened with TOKEN_DUPLICATE, and
    // `raw` is writable. `AccessCheck` needs an impersonation token, and
    // identification is the lowest level that allows the check.
    if unsafe {
        DuplicateToken(
            primary.as_raw_handle() as HANDLE,
            SecurityIdentification,
            &mut raw,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success returned a new token handle that this value now owns.
    let token = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };
    let mut descriptor = SECURITY_DESCRIPTOR {
        Revision: 0,
        Sbz1: 0,
        Control: 0,
        Owner: std::ptr::null_mut(),
        Group: std::ptr::null_mut(),
        Sacl: std::ptr::null_mut(),
        Dacl: std::ptr::null_mut(),
    };
    let pointer: PSECURITY_DESCRIPTOR = std::ptr::from_mut(&mut descriptor).cast();
    // SAFETY: `pointer` is a live, writable absolute descriptor. It stores
    // `owner` and `acl` by reference, and both outlive its only use below.
    // `AccessCheck` needs a group; the owner stands in, as no kept entry
    // that applies to the folder itself names the creator group.
    unsafe {
        if InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) == 0
            || SetSecurityDescriptorOwner(pointer, owner.as_psid(), 0) == 0
            || SetSecurityDescriptorGroup(pointer, owner.as_psid(), 0) == 0
            || SetSecurityDescriptorDacl(pointer, 1, acl.as_mut_ptr().cast(), 0) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let mut privileges = [0u64; PRIVILEGE_BYTES / size_of::<u64>()];
    let mut privilege_length =
        u32::try_from(PRIVILEGE_BYTES).map_err(|_| invalid_data("privilege buffer too large"))?;
    let (mut granted, mut status) = (0u32, 0i32);
    // SAFETY: the descriptor, token, and mapping are valid for this
    // synchronous call; `privileges` is aligned and writable for
    // `privilege_length` bytes, and the two outputs are writable.
    if unsafe {
        AccessCheck(
            pointer,
            token.as_raw_handle() as HANDLE,
            PRIVATE_DIRECTORY_ACCESS,
            &FILE_MAPPING,
            privileges.as_mut_ptr().cast(),
            &mut privilege_length,
            &mut granted,
            &mut status,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(status != 0 && granted & PRIVATE_DIRECTORY_ACCESS == PRIVATE_DIRECTORY_ACCESS)
}

/// Clears the inherited flag so the entry survives a protected DACL.
fn explicit(mut bytes: Vec<u8>) -> Vec<u8> {
    // `AceFlags` is the second byte of `ACE_HEADER`.
    if let Some(flags) = bytes.get_mut(1) {
        *flags &= !INHERITED_FLAG;
    }
    bytes
}

/// Copies each ACE's bytes, in DACL order, by its own `AceSize`.
#[allow(unsafe_code)]
fn raw_aces(security: &ObjectSecurity) -> io::Result<Vec<Vec<u8>>> {
    let dacl = security.dacl();
    let mut information = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    let length = u32::try_from(size_of::<ACL_SIZE_INFORMATION>())
        .map_err(|_| invalid_data("ACL information too large"))?;
    // SAFETY: `ObjectSecurity::read` refused a null DACL, `dacl` points into
    // the descriptor that `security` keeps alive, and `information` is
    // writable for `length` bytes.
    let read = unsafe {
        GetAclInformation(
            dacl,
            std::ptr::from_mut(&mut information).cast(),
            length,
            AclSizeInformation,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    if information.AceCount > MAX_ROOT_ACES {
        return Err(invalid_data("object DACL has too many entries"));
    }
    let in_use = usize::try_from(information.AclBytesInUse)
        .map_err(|_| invalid_data("ACL size out of range"))?;
    let mut aces = Vec::new();
    for index in 0..information.AceCount {
        let mut slot = MaybeUninit::<*mut c_void>::uninit();
        // SAFETY: `index` is below the reported ACE count, and `slot` is
        // writable. The returned pointer refers into `dacl`.
        if unsafe { GetAce(dacl, index, slot.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful `GetAce` has written the output pointer.
        let raw = unsafe { slot.assume_init() };
        if raw.is_null() {
            return Err(invalid_data("native ACE pointer is null"));
        }
        let offset = raw.cast::<u8>().addr().checked_sub(dacl.addr());
        if offset
            .and_then(|offset| offset.checked_add(size_of::<ACE_HEADER>()))
            .is_none_or(|end| end > in_use)
        {
            return Err(invalid_data("native ACE lies outside its DACL"));
        }
        // SAFETY: `raw` is non-null, and the check above places its header
        // inside the bytes the live DACL uses.
        let header = unsafe { std::ptr::read_unaligned(raw.cast::<ACE_HEADER>()) };
        let size = usize::from(header.AceSize);
        let end = offset.and_then(|offset| offset.checked_add(size));
        if size < size_of::<ACE_HEADER>() || end.is_none_or(|end| end > in_use) {
            return Err(invalid_data("native ACE lies outside its DACL"));
        }
        let mut bytes = vec![0u8; size];
        // SAFETY: the check above places all `size` bytes of this ACE inside
        // the live DACL, and `bytes` holds exactly that size.
        unsafe {
            std::ptr::copy_nonoverlapping(raw.cast::<u8>(), bytes.as_mut_ptr(), size);
        }
        aces.push(bytes);
    }
    Ok(aces)
}

/// An ACL holding exactly `keep`, in order, in aligned storage.
#[allow(unsafe_code)]
fn build_acl(keep: &[Vec<u8>]) -> io::Result<Vec<u64>> {
    let mut bytes = size_of::<ACL>();
    for ace in keep {
        bytes = bytes
            .checked_add(ace.len())
            .ok_or_else(|| invalid_data("ACL size overflow"))?;
    }
    let length = u32::try_from(bytes).map_err(|_| invalid_data("ACL too large"))?;
    let mut storage = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    let acl = storage.as_mut_ptr().cast::<ACL>();
    // SAFETY: `storage` is aligned and sized for the header and every ACE.
    // `AddAce` copies each ACE, whose length is its own `AceSize`.
    unsafe {
        if InitializeAcl(acl, length, ACL_REVISION) == 0 {
            return Err(io::Error::last_os_error());
        }
        for ace in keep {
            let size = u32::try_from(ace.len()).map_err(|_| invalid_data("ACE too large"))?;
            if AddAce(acl, ACL_REVISION, MAXDWORD, ace.as_ptr().cast(), size) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(storage)
}

/// Writes `acl` as the folder's protected DACL.
#[allow(unsafe_code)]
fn apply(file: &File, acl: &mut [u64]) -> io::Result<()> {
    let acl = acl.as_mut_ptr().cast::<ACL>();
    // SAFETY: `file` is a live handle opened with WRITE_DAC, `acl` is a valid
    // ACL from `build_acl` that outlives this synchronous call, and the owner,
    // group, and SACL are not changed.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).map_err(|_| invalid_data("native error out of range"))?,
        ));
    }
    Ok(())
}

/// The SID's string form and, when Windows can resolve it, its account name.
#[allow(unsafe_code)]
fn describe(sid: &Sid) -> io::Result<ExtraGrant> {
    let mut text = std::ptr::null_mut();
    // SAFETY: `sid` holds a valid SID and `text` is writable.
    if unsafe { ConvertSidToStringSidW(sid.as_psid(), &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success returned a NUL-terminated wide string that is read
    // here and then freed once with `LocalFree`, as documented.
    let value = unsafe {
        let length = (0..).take_while(|index| *text.add(*index) != 0).count();
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        // A failed free can only leak this short string; the SID text is
        // already copied, so there is nothing to recover.
        let _ = LocalFree(text.cast());
        value
    };
    Ok(ExtraGrant {
        sid: value,
        account: account_name(sid),
    })
}

/// `DOMAIN\name`, or `None` when the SID cannot be resolved.
#[allow(unsafe_code)]
fn account_name(sid: &Sid) -> Option<String> {
    let mut name = [0u16; ACCOUNT_UNITS];
    let mut domain = [0u16; ACCOUNT_UNITS];
    // Each length comes from its own buffer, so the two cannot drift apart.
    let mut name_units = u32::try_from(name.len()).ok()?;
    let mut domain_units = u32::try_from(domain.len()).ok()?;
    let mut usage: SID_NAME_USE = 0;
    // SAFETY: each buffer is writable for the unit count passed with it,
    // which was taken from that buffer's length, and the SID is valid for
    // this synchronous call.
    let found = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid.as_psid(),
            name.as_mut_ptr(),
            &mut name_units,
            domain.as_mut_ptr(),
            &mut domain_units,
            &mut usage,
        )
    };
    if found == 0 {
        return None;
    }
    let name = String::from_utf16_lossy(name.get(..usize::try_from(name_units).ok()?)?);
    let domain = String::from_utf16_lossy(domain.get(..usize::try_from(domain_units).ok()?)?);
    Some(if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    })
}

#[cfg(test)]
mod tests;
