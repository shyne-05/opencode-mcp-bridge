use std::{env, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=MCP_BUILD_COMMIT_OVERRIDE");
    println!("cargo:rerun-if-env-changed=MCP_BUILD_DIRTY_OVERRIDE");

    let commit = env::var("MCP_BUILD_COMMIT_OVERRIDE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "unknown".into());
    let dirty = env::var("MCP_BUILD_DIRTY_OVERRIDE")
        .ok()
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or_else(|| git(&["status", "--porcelain"]).is_some_and(|value| !value.is_empty()));

    println!("cargo:rustc-env=MCP_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=MCP_BUILD_DIRTY={dirty}");
}
