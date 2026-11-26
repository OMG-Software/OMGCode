//! Detector tests on fixture trees laid out as Windows and Linux installs.
//! Detectors read the host's platform, not the build's, so both layouts run
//! on either CI host.

use super::*;
use crate::control::{
    MAX_PROFILE_PATH_ENTRIES, MAX_PROFILE_ROOTS, MAX_PROFILE_VARIABLES, MAX_VARIABLE_VALUE_BYTES,
    is_reserved_variable, valid_profile_name,
};
use tempfile::TempDir;

struct Tree {
    dir: TempDir,
}

impl Tree {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("fixture tree"),
        }
    }

    /// The fixture root. On Unix it is resolved, because discovery resolves
    /// programs found on `PATH`.
    fn base(&self) -> PathBuf {
        if cfg!(unix) {
            fs::canonicalize(self.dir.path()).expect("resolved fixture root")
        } else {
            self.dir.path().to_path_buf()
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        joined_path(&self.base(), relative)
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    fn file(&self, relative: &str) -> PathBuf {
        self.text(relative, "")
    }

    fn text(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("fixture parent");
        fs::write(&path, contents).expect("fixture file");
        path
    }

    fn windows(&self) -> DiscoveryHost {
        self.dir("Windows/System32");
        DiscoveryHost::rooted_at(ProfilePlatform::Windows, &self.base())
    }

    fn linux(&self) -> DiscoveryHost {
        self.dir("usr/bin");
        DiscoveryHost::rooted_at(ProfilePlatform::Linux, &self.base())
    }

    /// An MSVC 14.50 toolset beside an older one, and a Windows SDK whose
    /// newest version lacks x64 `ucrt` libraries.
    fn msvc(&self) {
        for version in ["14.44.35207", "14.50.35717"] {
            self.file(&format!(
                "Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/{version}/bin/Hostx64/x64/cl.exe"
            ));
        }
        let kits = "Program Files (x86)/Windows Kits/10";
        self.dir(&format!("{kits}/Lib/10.0.26100.0/um/x64"));
        self.dir(&format!("{kits}/Lib/10.0.26100.0/ucrt/x64"));
        self.dir(&format!("{kits}/Lib/10.0.99999.0/um/x64"));
        self.dir(&format!("{kits}/bin/10.0.26100.0/x64"));
    }

    fn tools(&self) -> PathBuf {
        self.path("Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.50.35717")
    }

    fn kits(&self) -> PathBuf {
        self.path("Program Files (x86)/Windows Kits/10")
    }
}

fn named<'a>(proposals: &'a [ToolchainProposal], name: &str) -> &'a ToolchainProposal {
    proposals
        .iter()
        .find(|proposal| proposal.name() == name)
        .unwrap_or_else(|| panic!("no proposal {name}: {:?}", names(proposals)))
}

fn names(proposals: &[ToolchainProposal]) -> Vec<&str> {
    proposals.iter().map(ToolchainProposal::name).collect()
}

fn of_family(proposals: &[ToolchainProposal], family: ToolchainFamily) -> Vec<&str> {
    proposals
        .iter()
        .filter(|proposal| proposal.family() == family)
        .map(ToolchainProposal::name)
        .collect()
}

fn variable<'a>(proposal: &'a ToolchainProposal, name: &str) -> Option<&'a str> {
    proposal
        .environment()
        .iter()
        .find(|(variable, _)| variable == name)
        .map(|(_, value)| value.as_str())
}

fn variable_names(proposal: &ToolchainProposal) -> Vec<&str> {
    proposal
        .environment()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect()
}

#[test]
fn windows_rust_uses_the_toolchain_directory_with_msvc_and_never_the_cargo_home() {
    let tree = Tree::new();
    let host = tree.windows();
    tree.msvc();
    let rustup = "Users/operator/.rustup";
    tree.text(
        &format!("{rustup}/settings.toml"),
        "version = \"12\"\ndefault_toolchain = \"stable-x86_64-pc-windows-msvc\"\n",
    );
    for toolchain in [
        "stable-x86_64-pc-windows-msvc",
        "1.99.1-x86_64-pc-windows-msvc",
    ] {
        tree.file(&format!("{rustup}/toolchains/{toolchain}/bin/rustc.exe"));
    }
    tree.dir(&format!("{rustup}/toolchains/incomplete/bin"));
    tree.file("Users/operator/.cargo/bin/cargo.exe");
    tree.text(
        "Users/operator/.cargo/credentials.toml",
        "token = \"secret\"",
    );

    let proposals = discover(&host.with_search_path(vec![tree.path("Users/operator/.cargo/bin")]));
    assert_eq!(
        of_family(&proposals, ToolchainFamily::Rust),
        ["rust", "rust-1.99.1-x86_64-pc-windows-msvc"]
    );
    let rust = named(&proposals, "rust");
    let toolchain = tree.path(&format!(
        "{rustup}/toolchains/stable-x86_64-pc-windows-msvc"
    ));
    let system = tree.path("Windows/System32");
    assert_eq!(
        rust.read_only_roots(),
        [toolchain.clone(), tree.tools(), tree.kits(), system.clone()]
    );
    assert_eq!(
        rust.path_entries(),
        [
            toolchain.join("bin"),
            tree.tools().join("bin").join("Hostx64").join("x64"),
            tree.kits().join("bin").join("10.0.26100.0").join("x64"),
            system,
        ]
    );
    assert_eq!(variable_names(rust), ["INCLUDE", "LIB", "VCINSTALLDIR"]);
    assert!(rust.notes().contains(&ProposalNote::OfflineDependencies));
    assert!(!rust.notes().contains(&ProposalNote::NoMsvc));
    assert!(!rust.notes().contains(&ProposalNote::NotQualified));
    for proposal in &proposals {
        assert!(
            proposal
                .read_only_roots()
                .iter()
                .all(|root| !root.starts_with(tree.path("Users/operator/.cargo"))),
            "{proposal:?}"
        );
        assert!(
            proposal
                .environment()
                .iter()
                .all(|(name, _)| !name.starts_with("RUSTUP") && name != "CARGO_HOME")
        );
    }
}

#[test]
fn a_short_default_toolchain_name_selects_its_directory() {
    let tree = Tree::new();
    let host = tree.linux();
    let rustup = "home/operator/.rustup";
    tree.text(
        &format!("{rustup}/settings.toml"),
        "default_toolchain = \"stable\"\n",
    );
    for toolchain in [
        "1.99.1-x86_64-unknown-linux-gnu",
        "stable-x86_64-unknown-linux-gnu",
    ] {
        tree.file(&format!("{rustup}/toolchains/{toolchain}/bin/rustc"));
    }
    let proposals = discover(&host);
    let rust = named(&proposals, "rust");
    assert_eq!(rust.version(), Some("stable-x86_64-unknown-linux-gnu"));
    assert!(variable_names(rust).is_empty());
    named(&proposals, "rust-1.99.1-x86_64-unknown-linux-gnu");
}

#[test]
fn a_windows_msvc_rust_toolchain_without_msvc_says_linking_fails() {
    let tree = Tree::new();
    let host = tree.windows();
    tree.file("Users/operator/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe");
    tree.file("Users/operator/.rustup/toolchains/stable-x86_64-pc-windows-gnu/bin/rustc.exe");
    let proposals = discover(&host);
    let msvc = proposals
        .iter()
        .find(|p| p.version() == Some("stable-x86_64-pc-windows-msvc"))
        .expect("msvc toolchain");
    assert!(msvc.notes().contains(&ProposalNote::NoMsvc));
    assert_eq!(msvc.read_only_roots().len(), 2);
    let gnu = proposals
        .iter()
        .find(|p| p.version() == Some("stable-x86_64-pc-windows-gnu"))
        .expect("gnu toolchain");
    assert!(!gnu.notes().contains(&ProposalNote::NoMsvc));
}

#[test]
fn msvc_uses_the_newest_toolset_and_the_newest_sdk_with_x64_libraries() {
    let tree = Tree::new();
    let host = tree.windows();
    tree.msvc();
    let proposals = discover(&host);
    assert_eq!(of_family(&proposals, ToolchainFamily::CCpp), ["msvc"]);
    let msvc = named(&proposals, "msvc");
    assert_eq!(
        msvc.version(),
        Some("MSVC 14.50.35717, Windows SDK 10.0.26100.0")
    );
    let tools = tree.tools();
    let include = tree.kits().join("Include").join("10.0.26100.0");
    let lib = tree.kits().join("Lib").join("10.0.26100.0");
    let list = |paths: &[PathBuf]| {
        paths
            .iter()
            .map(|path| path.to_str().expect("UTF-8").to_owned())
            .collect::<Vec<_>>()
            .join(";")
    };
    assert_eq!(
        variable(msvc, "INCLUDE"),
        Some(
            list(&[
                tools.join("include"),
                include.join("ucrt"),
                include.join("um"),
                include.join("shared"),
            ])
            .as_str()
        )
    );
    assert_eq!(
        variable(msvc, "LIB"),
        Some(
            list(&[
                tools.join("lib").join("x64"),
                lib.join("ucrt").join("x64"),
                lib.join("um").join("x64"),
            ])
            .as_str()
        )
    );
    let vc = tree.path("Program Files/Microsoft Visual Studio/18/Community/VC");
    assert_eq!(
        variable(msvc, "VCINSTALLDIR"),
        Some(format!("{}\\", vc.to_str().expect("UTF-8")).as_str())
    );
    assert!(!msvc.notes().contains(&ProposalNote::NotQualified));
}

#[test]
fn windows_clang_and_mingw_gcc_are_proposed_and_marked_unqualified() {
    let tree = Tree::new();
    let host = tree.windows();
    tree.msvc();
    tree.file("Program Files/LLVM/bin/clang.exe");
    tree.dir("Program Files/LLVM/lib/clang/19");
    tree.file("msys64/ucrt64/bin/gcc.exe");
    tree.dir("msys64/ucrt64/lib/gcc/x86_64-w64-mingw32/14.2.0");
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::CCpp),
        ["msvc", "clang", "gcc"]
    );
    let clang = named(&proposals, "clang");
    assert_eq!(clang.version(), Some("19"));
    assert_eq!(
        clang.read_only_roots()[..3],
        [tree.path("Program Files/LLVM"), tree.tools(), tree.kits()]
    );
    assert!(clang.notes().contains(&ProposalNote::NotQualified));
    let gcc = named(&proposals, "gcc");
    assert_eq!(gcc.version(), Some("14.2.0"));
    assert_eq!(
        gcc.read_only_roots(),
        [tree.path("msys64/ucrt64"), tree.path("Windows/System32")]
    );
    assert!(gcc.notes().contains(&ProposalNote::NotQualified));
}

#[test]
fn java_homes_set_java_home_and_are_named_by_major_version() {
    let tree = Tree::new();
    let host = tree.windows();
    let jdk = tree.dir("Program Files/Eclipse Adoptium/jdk-21.0.2.13-hotspot");
    tree.file("Program Files/Eclipse Adoptium/jdk-21.0.2.13-hotspot/bin/java.exe");
    tree.file("Program Files/Eclipse Adoptium/jdk-21.0.2.13-hotspot/bin/javac.exe");
    tree.text(
        "Program Files/Eclipse Adoptium/jdk-21.0.2.13-hotspot/release",
        "IMPLEMENTOR=\"Eclipse Adoptium\"\nJAVA_VERSION=\"21.0.2\"\n",
    );
    tree.file("Program Files/Java/jre-17/bin/java.exe");
    tree.text(
        "Program Files/Java/jre-17/release",
        "JAVA_VERSION=\"17.0.1\"\n",
    );
    tree.dir("Program Files/Microsoft/Edge");
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::Java),
        ["java", "java-17"]
    );
    let java = named(&proposals, "java");
    assert_eq!(java.version(), Some("21.0.2"));
    assert_eq!(variable(java, "JAVA_HOME"), jdk.to_str());
    assert_eq!(java.path_entries()[0], jdk.join("bin"));
    assert!(!java.notes().contains(&ProposalNote::JavaRuntimeOnly));
    assert!(
        named(&proposals, "java-17")
            .notes()
            .contains(&ProposalNote::JavaRuntimeOnly)
    );
}

#[test]
fn windows_python_needs_its_standard_library_and_skips_shims_aliases_and_venvs() {
    let tree = Tree::new();
    let local = "Users/operator/AppData/Local";
    let user = tree.dir(&format!("{local}/Programs/Python/Python313"));
    tree.file(&format!("{local}/Programs/Python/Python313/python.exe"));
    tree.file(&format!("{local}/Programs/Python/Python313/python313.dll"));
    tree.dir(&format!("{local}/Programs/Python/Python313/Lib"));
    tree.dir(&format!("{local}/Programs/Python/Python313/Scripts"));
    let system = tree.dir("Python314");
    tree.file("Python314/python.exe");
    tree.file("Python314/python314.dll");
    tree.dir("Python314/Lib");
    let shim = tree.dir(&format!("{local}/Python/bin"));
    tree.file(&format!("{local}/Python/bin/python.exe"));
    let alias = tree.dir(&format!("{local}/Microsoft/WindowsApps"));
    tree.file(&format!("{local}/Microsoft/WindowsApps/python.exe"));
    let venv = tree.dir("work/.venv/Scripts");
    tree.file("work/.venv/Scripts/python.exe");
    tree.file("work/.venv/pyvenv.cfg");
    let host = tree
        .windows()
        .with_search_path(vec![shim, alias, venv, system.clone()]);
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::Python),
        ["python", "python-3.13"]
    );
    assert_eq!(named(&proposals, "python").read_only_roots()[0], system);
    let older = named(&proposals, "python-3.13");
    assert_eq!(older.version(), Some("3.13"));
    assert_eq!(
        older.path_entries(),
        [
            user.clone(),
            user.join("Scripts"),
            tree.path("Windows/System32")
        ]
    );
}

#[test]
fn strawberry_perl_lists_its_bins_and_git_perl_is_skipped() {
    let tree = Tree::new();
    tree.file("Strawberry/perl/bin/perl.exe");
    tree.dir("Strawberry/perl/site/bin");
    tree.dir("Strawberry/c/bin");
    let git = tree.dir("Program Files/Git/usr/bin");
    tree.file("Program Files/Git/usr/bin/perl.exe");
    let host = tree.windows().with_search_path(vec![git]);
    let proposals = discover(&host);
    assert_eq!(of_family(&proposals, ToolchainFamily::Perl), ["perl"]);
    let perl = named(&proposals, "perl");
    assert_eq!(
        perl.read_only_roots(),
        [tree.path("Strawberry"), tree.path("Windows/System32")]
    );
    assert_eq!(
        perl.path_entries()[..3],
        [
            tree.path("Strawberry/perl/site/bin"),
            tree.path("Strawberry/perl/bin"),
            tree.path("Strawberry/c/bin"),
        ]
    );
    // Strawberry's MinGW is a C compiler too, but only through `PATH`.
    assert!(of_family(&proposals, ToolchainFamily::CCpp).is_empty());
}

#[test]
fn go_sets_its_root_and_never_switches_toolchains() {
    let tree = Tree::new();
    let host = tree.windows();
    let go = tree.dir("Program Files/Go");
    tree.file("Program Files/Go/bin/go.exe");
    tree.text(
        "Program Files/Go/VERSION",
        "go1.27.4\ntime 2026-10-01T00:00:00Z\n",
    );
    let proposals = discover(&host.with_goroot(go.clone()));
    assert_eq!(of_family(&proposals, ToolchainFamily::Go), ["go"]);
    let proposal = named(&proposals, "go");
    assert_eq!(proposal.version(), Some("1.27.4"));
    assert_eq!(variable(proposal, "GOROOT"), go.to_str());
    assert_eq!(variable(proposal, "GOTOOLCHAIN"), Some("local"));
    assert_eq!(variable(proposal, "GOENV"), Some("off"));
    assert_eq!(variable(proposal, "GOFLAGS"), Some("-buildvcs=false"));
}

#[test]
fn windows_node_preserves_symlinks_and_skips_shim_folders() {
    let tree = Tree::new();
    tree.file("Program Files/nodejs/node.exe");
    tree.dir("Program Files/nodejs/node_modules/npm");
    let nvm = "Users/operator/AppData/Roaming/nvm/v20.11.0";
    tree.file(&format!("{nvm}/node.exe"));
    tree.dir(&format!("{nvm}/node_modules"));
    let volta = tree.dir("Users/operator/AppData/Local/Volta/bin");
    tree.file("Users/operator/AppData/Local/Volta/bin/node.exe");
    let host = tree.windows().with_search_path(vec![volta]);
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::Node),
        ["node", "node-2"]
    );
    for proposal in proposals
        .iter()
        .filter(|p| p.family() == ToolchainFamily::Node)
    {
        assert_eq!(
            variable(proposal, "NODE_OPTIONS"),
            Some("--preserve-symlinks --preserve-symlinks-main")
        );
        assert!(
            proposal
                .notes()
                .contains(&ProposalNote::NodeWindowsChildren)
        );
    }
    assert_eq!(named(&proposals, "node").version(), Some("20.11.0"));
}

#[test]
fn dotnet_reports_its_sdk_or_that_it_is_a_runtime() {
    let tree = Tree::new();
    let host = tree.windows();
    let machine = tree.dir("Program Files/dotnet");
    tree.file("Program Files/dotnet/dotnet.exe");
    tree.dir("Program Files/dotnet/shared/Microsoft.NETCore.App/9.0.1");
    tree.dir("Program Files/dotnet/sdk/9.0.100");
    tree.file("Users/operator/.dotnet/dotnet.exe");
    tree.dir("Users/operator/.dotnet/shared/Microsoft.NETCore.App/8.0.5");
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::DotNet),
        ["dotnet", "dotnet-8.0.5"]
    );
    let sdk = named(&proposals, "dotnet");
    assert_eq!(sdk.version(), Some("SDK 9.0.100"));
    assert_eq!(variable(sdk, "DOTNET_ROOT"), machine.to_str());
    assert_eq!(variable(sdk, "DOTNET_CLI_TELEMETRY_OPTOUT"), Some("1"));
    assert!(
        named(&proposals, "dotnet-8.0.5")
            .notes()
            .contains(&ProposalNote::DotnetRuntimeOnly)
    );
}

#[test]
fn windows_ruby_comes_from_rubyinstaller_folders() {
    let tree = Tree::new();
    let host = tree.windows();
    tree.file("Ruby33-x64/bin/ruby.exe");
    tree.dir("Ruby33-x64/lib/ruby/3.3.0");
    tree.dir("Ruby33-x64/lib/ruby/site_ruby");
    let proposals = discover(&host);
    let ruby = named(&proposals, "ruby");
    assert_eq!(ruby.version(), Some("3.3.0"));
    assert_eq!(ruby.path_entries()[0], tree.path("Ruby33-x64/bin"));
}

#[test]
fn linux_compilers_in_usr_share_the_system_roots() {
    let tree = Tree::new();
    let host = tree.linux();
    tree.dir("lib/x86_64-linux-gnu");
    tree.file("usr/bin/gcc");
    tree.dir("usr/lib/gcc/x86_64-linux-gnu/13");
    tree.file("usr/bin/clang");
    tree.dir("usr/lib/llvm-18");
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::CCpp),
        ["gcc", "clang"]
    );
    let gcc = named(&proposals, "gcc");
    assert_eq!(gcc.version(), Some("13"));
    assert_eq!(gcc.read_only_roots(), [tree.path("usr"), tree.path("lib")]);
    assert_eq!(gcc.path_entries(), [tree.path("usr/bin")]);
    assert!(!gcc.notes().contains(&ProposalNote::NotQualified));
    let clang = named(&proposals, "clang");
    assert_eq!(clang.version(), Some("18"));
    assert!(clang.notes().contains(&ProposalNote::NotQualified));
}

#[cfg(unix)]
#[test]
fn linked_top_level_directories_and_jdk_aliases_are_not_listed_twice() {
    let tree = Tree::new();
    let host = tree.linux();
    tree.dir("usr/lib64");
    std::os::unix::fs::symlink(tree.path("usr/lib64"), tree.path("lib64")).expect("link");
    let jdk = "usr/lib/jvm/java-21-openjdk-amd64";
    tree.file(&format!("{jdk}/bin/java"));
    tree.file(&format!("{jdk}/bin/javac"));
    std::os::unix::fs::symlink(tree.path(jdk), tree.path("usr/lib/jvm/default-java"))
        .expect("alias");
    let proposals = discover(&host);
    assert_eq!(of_family(&proposals, ToolchainFamily::Java), ["java"]);
    let java = named(&proposals, "java");
    assert_eq!(java.read_only_roots(), [tree.path("usr")]);
    assert_eq!(
        java.path_entries(),
        [tree.path(&format!("{jdk}/bin")), tree.path("usr/bin")]
    );
    assert_eq!(variable(java, "JAVA_HOME"), tree.path(jdk).to_str());
}

#[cfg(unix)]
#[test]
fn a_path_program_resolves_to_its_installation() {
    let tree = Tree::new();
    let host = tree.linux();
    // Not a standard Go location: only the link on `PATH` leads here.
    let go = "srv/toolchains/go";
    tree.file(&format!("{go}/bin/go"));
    tree.text(&format!("{go}/VERSION"), "go1.22.2\n");
    std::os::unix::fs::symlink(tree.path(&format!("{go}/bin/go")), tree.path("usr/bin/go"))
        .expect("link");
    assert!(of_family(&discover(&host), ToolchainFamily::Go).is_empty());
    let proposals = discover(&host.with_search_path(vec![tree.path("usr/bin")]));
    assert_eq!(of_family(&proposals, ToolchainFamily::Go), ["go"]);
    let proposal = named(&proposals, "go");
    assert_eq!(variable(proposal, "GOROOT"), tree.path(go).to_str());
    assert_eq!(
        proposal.read_only_roots(),
        [tree.path(go), tree.path("usr")]
    );
}

#[cfg(unix)]
#[test]
fn a_bin_at_the_filesystem_root_is_never_proposed_as_a_root() {
    let tree = Tree::new();
    let host = tree.linux();
    tree.file("bin/gcc");
    let proposals = discover(&host.with_search_path(vec![tree.path("bin")]));
    for proposal in &proposals {
        assert!(
            proposal
                .read_only_roots()
                .iter()
                .all(|root| *root != tree.base()),
            "{proposal:?}"
        );
    }
}

#[test]
fn a_windows_install_at_the_drive_root_is_never_proposed() {
    let tree = Tree::new();
    tree.file("python.exe");
    tree.dir("Lib");
    let host = tree.windows().with_search_path(vec![tree.base()]);
    assert!(of_family(&discover(&host), ToolchainFamily::Python).is_empty());
}

#[test]
fn linux_python_comes_from_usr_and_pyenv_and_skips_virtual_environments() {
    let tree = Tree::new();
    let host = tree.linux();
    tree.file("usr/bin/python3");
    tree.dir("usr/lib/python3.12");
    tree.dir("usr/lib/python3");
    let pyenv = "home/operator/.pyenv/versions/3.13.1";
    tree.file(&format!("{pyenv}/bin/python3"));
    tree.dir(&format!("{pyenv}/lib/python3.13"));
    let venv = "work/.venv";
    tree.file(&format!("{venv}/bin/python3"));
    tree.dir(&format!("{venv}/lib/python3.12"));
    tree.file(&format!("{venv}/pyvenv.cfg"));
    let proposals = discover(&host.with_search_path(vec![tree.path(&format!("{venv}/bin"))]));
    assert_eq!(
        of_family(&proposals, ToolchainFamily::Python),
        ["python", "python-3.12"]
    );
    assert_eq!(
        named(&proposals, "python").read_only_roots(),
        [tree.path(pyenv), tree.path("usr")]
    );
    assert_eq!(
        named(&proposals, "python-3.12").read_only_roots(),
        [tree.path("usr")]
    );
}

#[test]
fn a_distribution_node_also_reads_etc_ssl() {
    let tree = Tree::new();
    let host = tree.linux();
    tree.file("usr/bin/node");
    tree.dir("etc/ssl/certs");
    let nvm = "home/operator/.nvm/versions/node/v22.1.0";
    tree.file(&format!("{nvm}/bin/node"));
    let proposals = discover(&host);
    assert_eq!(
        of_family(&proposals, ToolchainFamily::Node),
        ["node", "node-2"]
    );
    let nvm_node = named(&proposals, "node");
    assert_eq!(nvm_node.version(), Some("22.1.0"));
    assert_eq!(
        nvm_node.read_only_roots(),
        [tree.path(nvm), tree.path("usr")]
    );
    assert_eq!(
        named(&proposals, "node-2").read_only_roots(),
        [tree.path("usr"), tree.path("etc/ssl")]
    );
    assert!(nvm_node.environment().is_empty());
}

#[test]
fn linux_go_perl_ruby_and_dotnet_are_found_in_their_standard_places() {
    let tree = Tree::new();
    let host = tree.linux();
    tree.file("usr/local/go/bin/go");
    tree.text("usr/local/go/VERSION", "go1.26.10\n");
    tree.file("usr/bin/perl");
    tree.dir("usr/share/perl/5.38.2");
    tree.file("usr/bin/ruby");
    tree.dir("usr/lib/ruby/3.2.0");
    tree.dir("usr/lib/ruby/vendor_ruby");
    tree.file("home/operator/.dotnet/dotnet");
    tree.dir("home/operator/.dotnet/shared/Microsoft.NETCore.App/8.0.5");
    tree.dir("home/operator/.dotnet/sdk/8.0.100");
    let proposals = discover(&host);
    assert_eq!(named(&proposals, "go").version(), Some("1.26.10"));
    assert_eq!(
        named(&proposals, "go").read_only_roots(),
        [tree.path("usr")]
    );
    assert_eq!(named(&proposals, "perl").version(), Some("5.38.2"));
    assert_eq!(named(&proposals, "ruby").version(), Some("3.2.0"));
    let dotnet = named(&proposals, "dotnet");
    assert_eq!(dotnet.version(), Some("SDK 8.0.100"));
    assert_eq!(
        dotnet.read_only_roots(),
        [tree.path("home/operator/.dotnet"), tree.path("usr")]
    );
}

/// A host with every family installed, for the invariants below.
fn everything() -> (Tree, Vec<ToolchainProposal>) {
    let tree = Tree::new();
    let host = tree.windows();
    tree.msvc();
    tree.file("Users/operator/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe");
    tree.file("Program Files/LLVM/bin/clang.exe");
    tree.file("Program Files/Java/jdk-21/bin/java.exe");
    tree.file("Python314/python.exe");
    tree.dir("Python314/Lib");
    tree.file("Strawberry/perl/bin/perl.exe");
    tree.file("Program Files/Go/bin/go.exe");
    tree.file("Program Files/Go/VERSION");
    tree.file("Program Files/nodejs/node.exe");
    tree.dir("Program Files/nodejs/node_modules");
    tree.file("Program Files/dotnet/dotnet.exe");
    tree.dir("Program Files/dotnet/shared");
    tree.file("Ruby33-x64/bin/ruby.exe");
    tree.dir("Ruby33-x64/lib/ruby");
    let proposals = discover(&host);
    (tree, proposals)
}

#[test]
fn every_family_has_a_detector_and_a_proposal_on_a_full_host() {
    let (_tree, proposals) = everything();
    for family in [
        ToolchainFamily::Rust,
        ToolchainFamily::CCpp,
        ToolchainFamily::Java,
        ToolchainFamily::Python,
        ToolchainFamily::Perl,
        ToolchainFamily::Go,
        ToolchainFamily::Node,
        ToolchainFamily::DotNet,
        ToolchainFamily::Ruby,
    ] {
        assert!(
            !of_family(&proposals, family).is_empty(),
            "{family:?}: {:?}",
            names(&proposals)
        );
    }
    assert_eq!(
        names(&proposals),
        [
            "rust", "msvc", "clang", "java", "python", "perl", "go", "node", "dotnet", "ruby"
        ]
    );
}

#[test]
fn every_proposal_meets_cores_shape_rules() {
    let (_tree, proposals) = everything();
    assert_core_shape(&proposals);
}

/// Discovery on this computer, for a manual check: run with `--ignored
/// --nocapture`. It only reads folders, and asserts the same shape rules as
/// the fixture tests; what it finds depends on what is installed.
#[test]
#[ignore = "reads this computer's install folders; run by hand"]
fn this_computer_yields_proposals_that_meet_cores_shape_rules() {
    let host = DiscoveryHost::from_environment().expect("Linux or Windows");
    let proposals = discover(&host);
    for proposal in &proposals {
        println!(
            "{} [{}] {:?} roots={:?} path={:?} variables={:?} notes={:?}",
            proposal.name(),
            proposal.family().label(),
            proposal.version(),
            proposal.read_only_roots(),
            proposal.path_entries(),
            proposal
                .environment()
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            proposal.notes(),
        );
    }
    assert_core_shape(&proposals);
    assert!(proposals.len() <= MAX_PROPOSALS);
}

fn assert_core_shape(proposals: &[ToolchainProposal]) {
    let mut seen: Vec<String> = Vec::new();
    for proposal in proposals {
        let name = proposal.name();
        assert!(valid_profile_name(name), "{name}");
        assert!(!seen.iter().any(|other| other.eq_ignore_ascii_case(name)));
        seen.push(name.to_owned());
        assert!((1..=MAX_PROFILE_ROOTS).contains(&proposal.read_only_roots().len()));
        assert!(proposal.path_entries().len() <= MAX_PROFILE_PATH_ENTRIES);
        for entry in proposal.path_entries() {
            assert!(
                proposal
                    .read_only_roots()
                    .iter()
                    .any(|root| entry.starts_with(root)),
                "{entry:?} in {proposal:?}"
            );
        }
        assert!(proposal.environment().len() <= MAX_PROFILE_VARIABLES);
        for (variable, value) in proposal.environment() {
            assert!(!is_reserved_variable(variable), "{variable}");
            assert!(value.len() <= MAX_VARIABLE_VALUE_BYTES && !value.contains('\0'));
        }
        let declaration = proposal.declaration();
        assert_eq!(declaration.name, name);
        assert_eq!(declaration.read_only_roots, proposal.read_only_roots());
    }
}

#[test]
fn digests_are_stable_distinct_and_cover_every_recorded_value() {
    let (_tree, proposals) = everything();
    let (_again, repeated) = everything();
    let digests: Vec<String> = proposals.iter().map(ToolchainProposal::digest).collect();
    for digest in &digests {
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
    }
    let mut unique = digests.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), digests.len());
    // Another tree has other paths, so the same names digest differently.
    assert_ne!(digests[0], repeated[0].digest());
    let base = proposals[0].clone();
    let mut renamed = base.clone();
    renamed.name.push('x');
    let mut rooted = base.clone();
    rooted.read_only_roots.push(PathBuf::from("/other"));
    let mut valued = base.clone();
    valued
        .environment
        .push(("EXTRA".to_owned(), "1".to_owned()));
    for changed in [renamed, rooted, valued] {
        assert_ne!(changed.digest(), base.digest());
    }
    assert_eq!(base.clone().digest(), base.digest());
}

#[test]
fn a_host_with_nothing_installed_proposes_nothing() {
    let tree = Tree::new();
    assert!(discover(&tree.windows()).is_empty());
    let other = Tree::new();
    assert!(discover(&other.linux()).is_empty());
}

#[test]
fn installations_of_one_name_are_bounded() {
    let tree = Tree::new();
    let host = tree.linux();
    for minor in 0..20 {
        tree.file(&format!(
            "home/operator/.rustup/toolchains/1.{minor}.0-x86_64-unknown-linux-gnu/bin/rustc"
        ));
    }
    let proposals = discover(&host);
    let rust = of_family(&proposals, ToolchainFamily::Rust);
    assert_eq!(rust.len(), MAX_PER_NAME);
    // Newest first.
    assert_eq!(rust[0], "rust");
    assert_eq!(
        named(&proposals, "rust").version(),
        Some("1.19.0-x86_64-unknown-linux-gnu")
    );
}

#[test]
fn names_keep_to_cores_rule_and_never_repeat() {
    assert_eq!(profile_name("java", "21"), "java-21");
    assert_eq!(
        profile_name("rust", "nightly 2026/01+x"),
        "rust-nightly-2026-01-x"
    );
    assert_eq!(profile_name("node", ""), "node");
    assert_eq!(profile_name("go", "..."), "go");
    let long = profile_name("rust", &"a".repeat(100));
    assert_eq!(long.len(), MAX_PROFILE_NAME_CHARS);
    assert!(valid_profile_name(&long));
    let taken = vec!["node".to_owned(), "NODE-2".to_owned()];
    assert_eq!(unique_name("node", &taken), "node-3");
    assert_eq!(
        unique_name(&long, std::slice::from_ref(&long)).len(),
        MAX_PROFILE_NAME_CHARS
    );
}

#[test]
fn versions_compare_by_their_numbers() {
    assert!(version_key("1.99.1") > version_key("1.9.2"));
    assert!(version_key("10.0.26100.0") > version_key("10.0.22621.0"));
    assert!(version_key("") < version_key("0"));
}
