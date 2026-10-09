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

/// The same backends, checked with `cfg(test)` on. A backend's test module is
/// otherwise never type-checked off its own OS, which is how a
/// `ParentRef: Debug` gap shipped through a green `xcheck` and broke the
/// Windows leg. `core` is left out on purpose: its dev-dependencies pull in
/// criterion, whose `alloca` build script needs a cross C toolchain.
const TEST_CHECKS: &[(&str, &str)] = &[
    ("everyfiles-backend-windows", "x86_64-pc-windows-msvc"),
    ("everyfiles-backend-windows", "i686-pc-windows-msvc"),
    ("everyfiles-backend-linux", "x86_64-unknown-linux-gnu"),
    ("everyfiles-backend-linux", "aarch64-unknown-linux-gnu"),
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
    // `--lib` for the packages whose dev-dependency tree cannot be built for a
    // foreign target, `--profile test --lib` for the backends so their test
    // modules are type-checked too. Neither links, so both stay cheap; ABI
    // layout and runtime behaviour still need CI on the matching OS.
    let runs = CHECKS
        .iter()
        .map(|&(pkg, target)| vec!["check", "-p", pkg, "--target", target, "--lib"])
        .chain(TEST_CHECKS.iter().map(|&(pkg, target)| {
            vec![
                "check",
                "-p",
                pkg,
                "--target",
                target,
                "--profile",
                "test",
                "--lib",
            ]
        }));
    for args in runs {
        eprintln!("==> cargo {}", args.join(" "));
        let status = Command::new(env!("CARGO")).args(args).status();
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
