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
    let status = Command::new(mc)
        .arg("-r")
        .arg(&out_dir)
        .arg("-h")
        .arg(&out_dir)
        .arg(manifest_dir.join("messages.mc"))
        .status()
        .expect("Failed to execute mc.exe");

    if !status.success() {
        panic!(
            "mc.exe compilation failed with exit code: {:?}",
            status.code()
        );
    }

    // 4. Compile the generated resource script into a .res file
    let status = Command::new(rc)
        .current_dir(&out_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg("messages.res")
        .arg("messages.rc")
        .status()
        .expect("Failed to execute rc.exe");

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

/// Finds a Windows SDK tool, preferring PATH and falling back to the newest
/// installed Windows 10/11 SDK for the host architecture.
fn find_windows_sdk_tool(name: &str) -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let host_arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "x64"
    };

    let program_files = env::var_os("ProgramFiles(x86)")
        .map_or_else(|| PathBuf::from(r"C:\Program Files (x86)"), PathBuf::from);
    let bin_root = program_files.join(Path::new(r"Windows Kits\10\bin"));

    let mut versions: Vec<PathBuf> = std::fs::read_dir(bin_root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    versions.sort();
    versions.reverse();

    for version in versions {
        for arch in [host_arch, "x64", "x86"] {
            let candidate = version.join(arch).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}
