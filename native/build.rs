use std::fs;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    if let Ok(head) = fs::read_to_string("../.git/HEAD") {
        if let Some(ref_path) = head.trim().strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=../.git/{ref_path}");
        }
    }

    if let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).output() {
        if output.status.success() {
            let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !commit.is_empty() {
                println!("cargo:rustc-env=MIRU_CLIENT_COMMIT={commit}");
            }
        }
    }

    tauri_build::build()
}
