//! Repair tests on real NTFS or ReFS directories, using `icacls` to add the
//! grants another program could leave behind.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::Security::{WinAnonymousSid, WinBuiltinUsersSid};
use windows_sys::Win32::Storage::FileSystem::FILE_ADD_FILE;

use super::*;
use crate::broker::windows::tests::create_junction_bounded;
use crate::private_store::windows::WindowsPrivateDirectory;
use crate::private_store::windows::tests::private_root;

/// The built-in Users group, named by SID so the test is language-neutral.
const USERS: &str = "S-1-5-32-545";
/// The anonymous logon, which these tests never run as.
const ANONYMOUS: &str = "S-1-5-7";

fn icacls(path: &Path, arguments: &[&str]) {
    let output = Command::new("icacls")
        .arg(path)
        .args(arguments)
        .output()
        .expect("run icacls");
    assert!(
        output.status.success(),
        "icacls {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A data root that inherits its DACL from a temporary directory, as the real
/// application data root inherits from `%LOCALAPPDATA%`.
fn inheriting_root(temporary: &tempfile::TempDir) -> PathBuf {
    let path = temporary.path().join("data");
    fs::create_dir(&path).expect("data root");
    path
}

fn aces(path: &Path) -> (bool, Vec<Ace>) {
    let file = open(path, INSPECT_ACCESS).expect("open for inspection");
    let security = ObjectSecurity::read(&file).expect("security");
    (
        security.dacl_protected().expect("control"),
        security.aces().expect("aces"),
    )
}

/// The process user, the folder owner, and every decoded entry, for a
/// failure message: the fixture's DACL can differ between Windows editions.
fn dacl_report(path: &Path) -> String {
    let principals = Principals::current().expect("principals");
    let name = |sid: &Sid| describe(sid).map_or_else(|error| error.to_string(), |grant| grant.sid);
    let file = open(path, INSPECT_ACCESS).expect("open for inspection");
    let security = ObjectSecurity::read(&file).expect("security");
    let mut report = format!(
        "user {}, owner {}, protected {:?}\n",
        name(&principals.user),
        security
            .owner()
            .map_or_else(|error| error.to_string(), |owner| name(&owner)),
        security.dacl_protected()
    );
    for ace in security.aces().expect("aces") {
        report.push_str(&match ace {
            Ace::Allowed { flags, mask, sid } => {
                format!("  allow {} flags {flags:#x} mask {mask:#x}\n", name(&sid))
            }
            Ace::Denied { flags, mask, sid } => {
                format!("  deny {} flags {flags:#x} mask {mask:#x}\n", name(&sid))
            }
            Ace::Other { flags } => format!("  other flags {flags:#x}\n"),
        });
    }
    report
}

fn allows(aces: &[Ace], sid: &Sid) -> bool {
    aces.iter()
        .any(|ace| matches!(ace, Ace::Allowed { sid: granted, .. } if granted.equals(sid)))
}

fn grants(path: &Path) -> Vec<ExtraGrant> {
    match inspect(path).expect("inspect") {
        RootAccess::Repairable(grants) => grants,
        other => panic!("expected extra grants: {other:?}"),
    }
}

#[test]
fn a_confirmed_repair_removes_extra_grants_and_keeps_trusted_and_deny_entries() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    icacls(&root, &["/grant", &format!("*{USERS}:(OI)(CI)(RX)")]);
    icacls(&root, &["/deny", &format!("*{ANONYMOUS}:(OI)(CI)(R)")]);
    // An ordinary child inherits the grant, as the data root's own
    // subdirectories do.
    let child = root.join("sessions");
    fs::create_dir(&child).expect("inheriting child");
    // A store child carries the exact protected DACL and must not change.
    let private_child = private_root(&root, "credentials-v1");
    let users = Sid::well_known(WinBuiltinUsersSid).expect("users");
    assert!(allows(&aces(&child).1, &users), "fixture child inherits");
    assert_eq!(
        WindowsPrivateDirectory::open(&root)
            .err()
            .expect("refused before repair")
            .kind(),
        io::ErrorKind::InvalidData
    );

    let found = grants(&root);
    assert!(found.iter().any(|grant| grant.sid == USERS), "{found:?}");
    remove(&root, &found).expect("repair");

    WindowsPrivateDirectory::open(&root).expect("the root is private after repair");
    assert_eq!(inspect(&root).expect("inspect"), RootAccess::Private);
    let (protected, root_aces) = aces(&root);
    assert!(protected, "the repaired DACL must not inherit");
    assert!(!allows(&root_aces, &users));
    let anonymous = Sid::well_known(WinAnonymousSid).expect("anonymous");
    assert!(
        matches!(root_aces.first(), Some(Ace::Denied { sid, .. }) if sid.equals(&anonymous)),
        "the deny entry is kept, first"
    );
    let principals = Principals::current().expect("principals");
    assert!(
        allows(&root_aces, &principals.user),
        "the user keeps access"
    );
    assert!(
        !allows(&aces(&child).1, &users),
        "the child loses the inherited grant"
    );
    WindowsPrivateDirectory::open(&root)
        .and_then(|root| root.open_dir(OsStr::new("credentials-v1")))
        .expect("the protected store child is unchanged");
    assert!(private_child.exists());
}

#[test]
fn a_grant_list_that_changed_after_confirmation_changes_nothing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    icacls(&root, &["/grant", &format!("*{USERS}:(RX)")]);
    let confirmed = grants(&root);
    icacls(&root, &["/grant", &format!("*{ANONYMOUS}:(RX)")]);
    assert!(matches!(
        remove(&root, &confirmed),
        Err(RootRepairError::Changed)
    ));
    let (protected, root_aces) = aces(&root);
    assert!(!protected, "nothing was rewritten");
    assert!(allows(
        &root_aces,
        &Sid::well_known(WinBuiltinUsersSid).expect("users")
    ));

    // A confirmed list naming more principals than the folder grants also
    // refuses, so the operator always saw exactly what is removed.
    let mut longer = grants(&root);
    longer.push(ExtraGrant {
        sid: "S-1-5-21-1-2-3-4".to_owned(),
        account: None,
    });
    assert!(matches!(
        remove(&root, &longer),
        Err(RootRepairError::Changed)
    ));
}

#[test]
fn a_private_root_reports_private_and_a_repair_changes_nothing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = private_root(temporary.path(), "data");
    assert_eq!(inspect(&root).expect("inspect"), RootAccess::Private);
    remove(&root, &[]).expect("nothing to remove");
    assert!(matches!(
        remove(
            &root,
            &[ExtraGrant {
                sid: USERS.to_owned(),
                account: None
            }]
        ),
        Err(RootRepairError::Changed)
    ));
    WindowsPrivateDirectory::open(&root).expect("still private");
}

/// A user entry that only children inherit grants nothing on the folder, so
/// removing the other grants would lock the user out and is refused.
#[test]
fn a_repair_that_would_leave_the_user_without_access_is_refused() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    let user = Principals::current().expect("principals").user;
    let user_sid = describe(&user).expect("user SID").sid;
    icacls(
        &root,
        &[
            "/inheritance:r",
            "/grant:r",
            &format!("*{user_sid}:(OI)(CI)(IO)(F)"),
            &format!("*{USERS}:(RX)"),
        ],
    );
    assert_eq!(
        inspect(&root).expect("inspect"),
        RootAccess::NotRepairable(NO_USER_ACCESS),
        "{}",
        dacl_report(&root)
    );
    assert!(matches!(
        remove(&root, &[]),
        Err(RootRepairError::NotRepairable(NO_USER_ACCESS))
    ));
}

/// A direct user entry that grants too little to open the store is not
/// enough: removing the group that gives the rest would lock the store out.
#[test]
fn a_repair_whose_user_entry_grants_too_little_is_refused() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    let user = Principals::current().expect("principals").user;
    let user_sid = describe(&user).expect("user SID").sid;
    icacls(
        &root,
        &[
            "/inheritance:r",
            "/grant:r",
            &format!("*{user_sid}:(RC)"),
            &format!("*{USERS}:(OI)(CI)(F)"),
        ],
    );
    assert_eq!(
        inspect(&root).expect("inspect"),
        RootAccess::NotRepairable(NO_USER_ACCESS),
        "{}",
        dacl_report(&root)
    );
    assert!(matches!(
        remove(&root, &grants_named(USERS)),
        Err(RootRepairError::NotRepairable(NO_USER_ACCESS))
    ));
    assert!(
        allows(
            &aces(&root).1,
            &Sid::well_known(WinBuiltinUsersSid).expect("users")
        ),
        "nothing was rewritten"
    );
}

/// A kept deny entry for a group the user belongs to counts against the
/// user, as Windows itself applies it.
#[test]
fn a_repair_that_a_kept_group_deny_would_lock_out_is_refused() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    icacls(&root, &["/grant", &format!("*{ANONYMOUS}:(RX)")]);
    // Add subdirectory, which the store needs to create its folders.
    icacls(&root, &["/deny", &format!("*{USERS}:(AD)")]);
    assert_eq!(
        inspect(&root).expect("inspect"),
        RootAccess::NotRepairable(NO_USER_ACCESS)
    );
    let (protected, _) = aces(&root);
    assert!(!protected, "nothing was rewritten");
}

/// Generic rights count as Windows maps them for files, so a user entry
/// written as `GENERIC_ALL` keeps the store's access.
#[test]
fn a_user_entry_with_only_generic_rights_still_allows_the_repair() {
    assert_eq!(
        map_generic(GENERIC_ALL) & PRIVATE_DIRECTORY_ACCESS,
        PRIVATE_DIRECTORY_ACCESS
    );
    assert_eq!(map_generic(GENERIC_READ) & FILE_ADD_FILE, 0);
    assert_eq!(map_generic(GENERIC_ALL) & GENERIC_ALL, 0);

    // The fixture writes raw generic entries, as another program could. NTFS
    // stores them mapped, so the check sees file rights either way; mapping
    // in the check keeps it right if a filesystem ever keeps them unmapped.
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    let user = Principals::current().expect("principals").user;
    let users = Sid::well_known(WinBuiltinUsersSid).expect("users");
    let mut fixture = build_acl(&[
        allowed_ace(&user, GENERIC_ALL),
        allowed_ace(&users, GENERIC_READ | GENERIC_EXECUTE),
    ])
    .expect("fixture ACL");
    apply(&open(&root, REPAIR_ACCESS).expect("open"), &mut fixture).expect("fixture DACL");
    assert!(
        aces(&root).1.iter().any(|ace| matches!(
            ace,
            Ace::Allowed { mask, sid, .. } if sid.equals(&user)
                && map_generic(*mask) & PRIVATE_DIRECTORY_ACCESS == PRIVATE_DIRECTORY_ACCESS
        )),
        "the fixture gives the user its generic entry"
    );

    let found = grants(&root);
    assert!(found.iter().any(|grant| grant.sid == USERS), "{found:?}");
    remove(&root, &found).expect("repair");
    WindowsPrivateDirectory::open(&root).expect("the store opens after repair");
}

/// An access-allowed ACE with no flags, as Windows lays one out.
fn allowed_ace(sid: &Sid, mask: u32) -> Vec<u8> {
    let length = sid.length().expect("SID length");
    let size = u16::try_from(8 + length).expect("ACE size");
    let mut ace = vec![0u8, 0u8];
    ace.extend_from_slice(&size.to_le_bytes());
    ace.extend_from_slice(&mask.to_le_bytes());
    ace.extend(
        sid.storage
            .iter()
            .flat_map(|word| word.to_ne_bytes())
            .take(length),
    );
    ace
}

fn grants_named(sid: &str) -> Vec<ExtraGrant> {
    vec![ExtraGrant {
        sid: sid.to_owned(),
        account: None,
    }]
}

#[test]
fn relative_paths_and_junctions_are_not_repaired() {
    assert_eq!(
        inspect(Path::new("relative")).expect_err("relative").kind(),
        io::ErrorKind::InvalidInput
    );
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = inheriting_root(&temporary);
    let link = temporary.path().join("link");
    create_junction_bounded(&link, &root).expect("junction");
    assert_eq!(
        inspect(&link).expect("inspect"),
        RootAccess::NotRepairable(NOT_A_FOLDER)
    );
    assert!(matches!(
        remove(&link, &[]),
        Err(RootRepairError::NotRepairable(NOT_A_FOLDER))
    ));
}
