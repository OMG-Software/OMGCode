# 0103: Generate toolchain profiles from installed toolchains

- Status: Accepted
- Date: 2026-11-26

## Context

A model command call can run only under a recorded toolchain profile
([ADR-0030](0030-run-trusted-commands-through-recorded-toolchain-profiles.md)).
An operator's chat failed with `toolchain profile is not recorded`: their
control journal had never recorded a profile, and the model guessed `rust`.
Writing a profile by hand means knowing every root, `PATH` entry, and
variable a toolchain needs, which the acceptance runs showed is more than its
executable directory. The `settings` window can add a profile
([ADR-0088](0088-change-execution-settings-in-the-settings-window.md)), but
only from hand-chosen folders.

[development-tools.md](../development-tools.md) allows later discovery in a
trusted host-side service that proposes resources for the operator to select,
and forbids repository-controlled discovery hooks and unconfined version
probes. Discovery must cover more than Rust, must grant nothing, and must
leave every record to the existing core calls, native confirmations, and the
confirmed settings revision.

## Decision

1. **A registry of detectors in core.** `omgcode_core::toolchain_discovery`
   holds one detector per family: Rust, C and C++ (MSVC with the Windows SDK,
   Clang, and MinGW GCC on Windows; GCC and Clang on Linux), Java, Python,
   Perl, Go, Node.js, .NET, and Ruby. Each returns candidates; a shared step
   completes, checks, names, orders, and bounds them. Adding a family is one
   detector and one registry entry. The host layout (Windows or Linux) is a
   value, not a compile-time choice, so every detector's Windows and Linux
   fixture trees run on both CI hosts.
2. **Discovery reads the filesystem only.** It lists the standard install
   folders, follows the operator's `PATH`, `JAVA_HOME`, `GOROOT`, and
   `RUSTUP_HOME` from the trusted process environment, and reads a few small
   files the installers wrote (rustup's `settings.toml`, a JDK's `release`,
   Go's `VERSION`), each at most 64 KiB. Versions come from those files and
   from folder names (`lib/gcc/<target>/<version>`, `python3NN.dll`, MSVC and
   Windows SDK version folders). It runs no program, confined or not, and
   reads no registry key. It never reads a workspace's files, so
   project-controlled configuration such as `rust-toolchain.toml`, `.nvmrc`,
   `.python-version`, or `global.json` cannot steer it. Listing is bounded to
   512 entries per folder and 256 `PATH` entries.
3. **What a proposal holds.** Each proposal has a name, a family, an optional
   version, roots, `PATH` entries, variables, and fixed notes.
   - Every proposal adds the system part the acceptance-tested profiles
     listed: `System32` on Windows; `/usr` and any real top-level `bin`,
     `lib`, `lib64`, or `lib32` on Linux.
   - A root is never a filesystem or drive root, nor the home directory or one
     of its ancestors. A symbolic link or junction is never a root. A nested
     or repeated root is dropped, and every `PATH` entry lies inside a root.
   - Only non-secret setup values are derived: `JAVA_HOME`, `GOROOT` with
     `GOTOOLCHAIN=local`, `GOENV=off`, and `GOFLAGS=-buildvcs=false`,
     `DOTNET_ROOT` with telemetry, logo, and certificate generation off,
     Node's symlink-preserving `NODE_OPTIONS`, and MSVC's `INCLUDE`, `LIB`,
     and `VCINSTALLDIR` for the x64 host and target. No reserved name is
     proposed, and core checks every value again.
   - A Rust proposal is one rustup toolchain directory, with its `bin` on
     `PATH`. It runs that toolchain's own `cargo` and `rustc`, not the rustup
     proxies, so it needs neither `RUSTUP_HOME` nor `RUSTUP_TOOLCHAIN`, and
     `~/.cargo`, which can hold registry credentials, is never a root. The
     rustup default toolchain is named `rust`. A Windows MSVC-target
     toolchain gets the MSVC and SDK roots and variables, or a note that none
     were found.
   - Names follow core's name rule: the newest or default installation of a
     family gets the bare name (`rust`, `go`, `java`, `python`), others a
     version suffix. At most 8 proposals share a base name and 32 are
     returned.
   - Notes say when a family has not passed the confined acceptance runs
     (only Rust, MSVC, Linux GCC, Python, Go, and Node.js have), when a JDK
     or .NET install is a runtime only, when Node on Windows needs
     `--test-isolation=none`, and that offline runs cannot download
     dependencies.
4. **Three `settings`-only commands; no path crosses the webview.**
   `settings_discover_toolchains` returns the proposals as display-safe text
   with a SHA-256 digest each. `settings_add_discovered_profiles` and
   `settings_grant_discovered_roots` take the settings revision and 1 to 8
   digests. Rust discovers again and applies only proposals whose digests
   still match; a selection made from an outdated list is refused as
   `discovery_changed`. Discovery runs before the settings store is opened.
5. **Core decides, before any dialog.** A new `ControlStore::check_profile`
   applies every rule of `add_profile` except the revision and records
   nothing. The list shows each proposal's refusal reason (for example, the
   name is taken, or 32 profiles exist), and an add whose selection core
   would refuse is refused before the confirmation. `add_profile` now gives
   the 32-profile limit its own reason.
6. **One native confirmation per batch.** Adding lists each selected
   profile's name, family, version, roots, `PATH` entries, and variables with
   their values, and says the list came from reading folders, no program ran,
   and a profile grants nothing. Unlike the hand-entered form, values are
   shown: discovery derived them from install paths, the operator typed none,
   and each is cut to 160 characters. Each profile is then recorded with
   `add_profile`, starting at the confirmed revision and advancing it once
   per record, so a change made while the dialog was open is refused as
   `stale_revision`. A failure after some records says how many were
   recorded; each record is valid alone, so nothing is rolled back.
7. **Windows container grants for the proposed roots.** With a workspace
   selected, each root says whether that workspace's AppContainer can read
   it (`readable`, as for `Program Files`), needs a read-execute grant this
   account can apply (`needs_grant`), or needs an administrator
   (`needs_administrator`). Rust reads the folder's access-control list for
   the derived container identity and opens the folder for `WRITE_DAC` to
   learn whether a grant could be applied; nothing changes. The grant command
   collects the `needs_grant` roots of the selection, drops folders beneath
   another, names the workspace and every folder in one native confirmation,
   lists the administrator-only folders it skips, and calls
   `grant_container_access` for each with the confirmed, advancing revision.
   Folders a grant would fail on are never offered, so no refused access-list
   change leaves a `pending` grant that blocks the workspace's runs. The list
   also says whether the workspace has the active read-write grant on its own
   folder that every run needs.

## Alternatives considered

- **Probe versions with `rustc -vV`, `java -version`, or `vswhere`.**
  Rejected for now. An unconfined probe is forbidden, and a confined one needs
  a recorded profile before discovery can propose one. Folder names and
  installer files gave every version the proposals need.
- **Read the Windows registry.** Rejected for now. Uninstall and vendor keys
  would find installs in custom folders, but they add a second untrusted
  input and an API surface the core does not need. Custom installs on `PATH`,
  `JAVA_HOME`, or `GOROOT` are found.
- **Read `rust-toolchain.toml` or similar project files** to choose the
  toolchain a project expects. Rejected: they are project-controlled.
- **Propose rustup proxies with `RUSTUP_HOME` and `RUSTUP_TOOLCHAIN`.**
  Rejected: the roots would include every installed toolchain, rustup's
  settings and downloads, and `~/.cargo`. One toolchain directory is narrower.
- **Record or grant automatically.** Rejected: discovery grants nothing, and
  each record keeps its native confirmation.
- **Send the selected roots back from the webview.** Rejected, as in
  ADR-0060 and ADR-0088: the window never names a path.
- **Keep the last list in Rust and select by index.** Rejected: re-discovery
  with digests keeps the service stateless and refuses a list that no longer
  matches the computer.
- **Offer every missing grant.** Rejected: a grant on an administrator-owned
  folder fails after the record, leaving a `pending` grant that blocks runs
  until revoked.

## Consequences

- An operator can record working profiles for the installed toolchains in a
  few clicks, and on Windows grant the folders they need, with every root,
  entry, and value shown first.
- A proposal is advice. Nothing was run to confirm it, a family that has not
  passed the acceptance runs may need more roots or variables, and the
  proposals are only as complete as the standard folders, `PATH`, and the
  three variables. Toolchains installed only in custom folders off `PATH` are
  not found.
- A local process that can write to the operator's home folders can make a
  folder look like a toolchain. It is proposed like any other, and the
  operator sees and confirms every root.
- The known limits of model runs stay: no network, a 20-second deadline, and
  1 GiB of memory. A `cargo` build that needs crates.io dependencies fails
  offline; only a trusted CLI `run --network`, under the workspace's
  `task_network` preference, can download them.
- No CLI command lists proposals yet. The desktop shell has run only on
  Windows; the Linux detectors are tested with fixture trees, not on a
  running Linux shell.
- Windows grant checks read real access-control lists; their integration
  tests stop at the confirmation and apply no grant.

## References

- [execution.md](../execution.md#toolchain-profiles)
- [development-tools.md](../development-tools.md#tool-sources)
- [desktop-interface.md](../desktop-interface.md)
- [ADR-0030](0030-run-trusted-commands-through-recorded-toolchain-profiles.md),
  [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md),
  [ADR-0088](0088-change-execution-settings-in-the-settings-window.md)
