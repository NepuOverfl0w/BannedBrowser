//! Embed the current Public Suffix List (<https://publicsuffix.org/list/>, MPL-2.0) in the
//! build. The list isn't kept in the repository: it changes often, and the browser downloads
//! a fresh copy at first start and then daily (`psl::refresh`). The copy embedded here makes
//! cookies correct before that first download.
//!
//! - `BB_PSL_FILE=<path>` embeds a local copy instead of downloading.
//! - `BB_PSL_OFFLINE=1` embeds nothing (cookies then stay host-only until the browser has
//!   downloaded the list itself).
//! - `BB_PSL_REQUIRED=1` fails the build instead of embedding nothing (release packages).
//!
//! The download uses the system `curl` (part of Windows 10+, macOS and Linux distributions),
//! which honours `HTTPS_PROXY`. If it fails, the build continues without a list and says so.

use std::path::{Path, PathBuf};
use std::process::Command;

const LIST_URL: &str = "https://publicsuffix.org/list/public_suffix_list.dat";

fn valid(text: &str) -> bool {
    text.contains("// ===BEGIN ICANN DOMAINS===")
        && text.contains("// ===END PRIVATE DOMAINS===")
        && text.lines().any(|l| l.starts_with("// VERSION:"))
}

fn download(dest: &Path) -> Result<String, String> {
    let tmp = dest.with_extension("download");
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "--max-time", "60", "-o"])
        .arg(&tmp)
        .arg(LIST_URL)
        .status()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed ({status})"));
    }
    let text = std::fs::read_to_string(&tmp).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BB_PSL_FILE");
    println!("cargo:rerun-if-env-changed=BB_PSL_OFFLINE");
    println!("cargo:rerun-if-env-changed=BB_PSL_REQUIRED");
    let required = std::env::var("BB_PSL_REQUIRED").is_ok_and(|v| v == "1");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let dest = out.join("public_suffix_list.dat");
    let text = if let Some(file) = std::env::var_os("BB_PSL_FILE") {
        println!("cargo:rerun-if-changed={}", Path::new(&file).display());
        std::fs::read_to_string(&file).map_err(|e| format!("BB_PSL_FILE: {e}"))
    } else if std::env::var("BB_PSL_OFFLINE").is_ok_and(|v| v == "1") {
        Err("BB_PSL_OFFLINE=1".to_string())
    } else {
        download(&dest)
    };
    if required {
        match &text {
            Ok(t) if valid(t) => {}
            Ok(_) => panic!("BB_PSL_REQUIRED=1, but the Public Suffix List isn't a valid list"),
            Err(e) => panic!("BB_PSL_REQUIRED=1, but no Public Suffix List: {e}"),
        }
    }
    let text = match text {
        Ok(t) if valid(&t) => t,
        Ok(_) => {
            println!(
                "cargo:warning=the Public Suffix List download wasn't a valid list; building without one"
            );
            String::new()
        }
        Err(e) => {
            println!(
                "cargo:warning=no Public Suffix List embedded ({e}); the browser downloads it at first start"
            );
            String::new()
        }
    };
    std::fs::write(&dest, text).expect("write OUT_DIR/public_suffix_list.dat");
}
