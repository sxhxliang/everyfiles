//! Repository automation. `cargo xtask xcheck` type-checks the platform
//! backends for the targets this host cannot execute.
//!
//! `cargo check --target` does not link, so it proves types, FFI signatures,
//! and cfg gating. It does NOT prove ABI layout, ioctl numbers, or runtime
//! behavior; real coverage for those comes from CI on the matching OS.

use std::process::{Command, ExitCode};

const CHECKS: &[(&str, &str)] = &[
    ("everyfiles-backend-windows", "x86_64-pc-windows-msvc"),
    ("everyfiles-backend-windows", "i686-pc-windows-msvc"),
    ("everyfiles-backend-linux", "x86_64-unknown-linux-gnu"),
    ("everyfiles-backend-linux", "aarch64-unknown-linux-gnu"),
    ("everyfiles-core", "x86_64-unknown-linux-gnu"),
    ("everyfiles-core", "x86_64-pc-windows-msvc"),
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "xcheck" => xcheck(),
        _ => {
            eprintln!("usage: cargo xtask xcheck");
            ExitCode::FAILURE
        }
    }
}

fn xcheck() -> ExitCode {
    let mut failed = false;
    for (pkg, target) in CHECKS {
        eprintln!("==> cargo check -p {pkg} --target {target}");
        let status = Command::new(env!("CARGO"))
            // Library targets only. Checking tests/benches would build
            // dev-dependencies for the foreign target, and some of those
            // (criterion's tree, for one) carry C build scripts that cannot
            // run without a cross C toolchain. Tests get real coverage in CI.
            .args(["check", "-p", pkg, "--target", target, "--lib"])
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("    FAILED ({s})");
                failed = true;
            }
            Err(e) => {
                eprintln!("    could not run cargo: {e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
