use std::{env, path::Path, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn track_git_path(name: &str, watch_missing_parent: bool) {
    let Some(path) = git(&["rev-parse", "--git-path", name]) else {
        return;
    };
    let mut path = Path::new(&path);
    if watch_missing_parent {
        // A packed branch may have no loose ref yet. Its parent is small and
        // captures later ref creation without watching the whole repository.
        while !path.exists() {
            let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            else {
                return;
            };
            path = parent;
        }
    }
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn track_provenance_inputs() {
    // Git resolves these paths for both ordinary checkouts and linked worktrees.
    for name in ["HEAD", "index", "packed-refs"] {
        track_git_path(name, false);
    }
    if let Some(reference) = git(&["symbolic-ref", "--quiet", "HEAD"]) {
        track_git_path(&reference, true);
    }

    // Explicit Cargo inputs otherwise stop source edits from rerunning this
    // script. Watch tracked and current untracked files, respecting .gitignore
    // so target/ never invalidates its own build.
    if let Ok(output) = Command::new("git")
        .arg("--no-optional-locks")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        && output.status.success()
    {
        for path in output.stdout.split(|byte| *byte == 0) {
            if let Ok(path) = std::str::from_utf8(path)
                && !path.is_empty()
                && !path.contains(['\n', '\r'])
            {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
}

fn main() {
    track_provenance_inputs();
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
