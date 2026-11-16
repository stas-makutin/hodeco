use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=messages.mc");

    // 1. Only compile resources when the compilation target is explicitly Windows
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }

    // 2. The Windows SDK tools (mc.exe / rc.exe) only run on Windows hosts
    if !cfg!(windows) {
        println!(
            "cargo:warning=Cross-compiling to Windows from a non-Windows host; \
             skipping messages.mc, so the Event Log message table will be missing."
        );
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mc = find_windows_sdk_tool("mc.exe")
        .expect("Failed to locate mc.exe. Ensure the Windows SDK / MSVC toolchain is installed.");
    let rc = find_windows_sdk_tool("rc.exe")
        .expect("Failed to locate rc.exe. Ensure the Windows SDK / MSVC toolchain is installed.");

    // 3. Compile the message text file into a MESSAGETABLE resource. mc.exe emits
    //    messages.rc plus the MSG00001.bin payload into OUT_DIR.
    let status = Command::new(&mc)
        .arg("-r")
        .arg(&out_dir)
        .arg("-h")
        .arg(&out_dir)
        .arg(manifest_dir.join("messages.mc"))
        .status()
        .unwrap_or_else(|err| {
            panic!(
                "Failed to execute {}: {err}\n\
                 Ensure the host-architecture Windows SDK tools are used. For ARM64 \
                 cross-builds, configure MSVC with `vcvarsall amd64_arm64`, not `arm64`.",
                mc.display()
            )
        });

    if !status.success() {
        panic!(
            "mc.exe compilation failed with exit code: {:?}",
            status.code()
        );
    }

    // 4. Compile the generated resource script into a .res file
    let status = Command::new(&rc)
        .current_dir(&out_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg("messages.res")
        .arg("messages.rc")
        .status()
        .unwrap_or_else(|err| {
            panic!(
                "Failed to execute {}: {err}\n\
                 Ensure the host-architecture Windows SDK tools are used. For ARM64 \
                 cross-builds, configure MSVC with `vcvarsall amd64_arm64`, not `arm64`.",
                rc.display()
            )
        });

    if !status.success() {
        panic!(
            "rc.exe compilation failed with exit code: {:?}",
            status.code()
        );
    }

    // 5. Instruct Cargo's linker to bake the compiled resource data into the binary
    println!(
        "cargo:rustc-link-arg-bins={}",
        out_dir.join("messages.res").display()
    );
}

/// Finds a Windows SDK tool for the host architecture.
///
/// Build scripts always run on the host, and resource compiler output does not
/// depend on the target architecture. `mc.exe` and `rc.exe` must therefore be
/// host binaries even when cross-compiling. In particular, an MSVC environment
/// configured with `vcvarsall arm64` puts ARM64 tools on `PATH`, which an x64
/// host cannot execute (Windows error 216).
///
/// The SDK directories are checked before `PATH` so that cross-compilation
/// environments cannot accidentally supply tools for the wrong architecture.
fn find_windows_sdk_tool(name: &str) -> Option<PathBuf> {
    let host_arch = host_tool_arch();

    // The SDK selected by the developer environment, if any.
    if let Some(bin) = env::var_os("WindowsSdkVerBinPath") {
        let candidate = PathBuf::from(bin).join(host_arch).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Any installed Windows 10/11 SDK, newest version first.
    let mut bin_roots = Vec::new();
    if let Some(sdk_dir) = env::var_os("WindowsSdkDir") {
        bin_roots.push(PathBuf::from(sdk_dir).join("bin"));
    }
    let program_files = env::var_os("ProgramFiles(x86)")
        .map_or_else(|| PathBuf::from(r"C:\Program Files (x86)"), PathBuf::from);
    bin_roots.push(program_files.join(Path::new(r"Windows Kits\10\bin")));

    for bin_root in bin_roots {
        if let Some(path) = find_tool_in_sdk_root(&bin_root, host_arch, name) {
            return Some(path);
        }
    }

    // Fall back to PATH for SDKs installed in non-standard locations.
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn find_tool_in_sdk_root(bin_root: &Path, host_arch: &str, name: &str) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = std::fs::read_dir(bin_root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    versions.sort();
    versions.reverse();

    for version in versions {
        for arch in compatible_tool_arches(host_arch) {
            let candidate = version.join(arch).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Returns the SDK tool architecture directories that can run on this host,
/// most preferred first.
fn compatible_tool_arches(host_arch: &str) -> &'static [&'static str] {
    match host_arch {
        // Windows on ARM can run x64 and x86 binaries under emulation.
        "arm64" => &["arm64", "x64", "x86"],
        // x64 Windows can run x86 binaries.
        "x64" => &["x64", "x86"],
        _ => &["x86"],
    }
}

/// Returns the host architecture used by the Windows SDK tool directories.
///
/// Build scripts are compiled for the host, so `std::env::consts::ARCH` is the
/// host architecture. `CARGO_CFG_TARGET_ARCH` is the cross-compilation target
/// and must not be used to choose an executable tool.
fn host_tool_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "x64",
    }
}
