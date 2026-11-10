//! Against GNU tar, where there is one: it lists what's written here, and
//! what it writes as ustar reads here.

use std::{path::PathBuf, process::Command};

use kobo_files::{Entry, read, write};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("kobo-files-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn has_tar() -> bool {
    Command::new("tar")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn gnu_tar_lists_and_unpacks_what_is_written() {
    if !has_tar() {
        return;
    }
    let dir = scratch("written");
    let long = format!("{}/{}", "x".repeat(120), "file");
    let entries = vec![
        Entry {
            path: ".adds/simple-books/reader.sh".to_owned(),
            mode: 0o755,
            bytes: b"#!/bin/sh\n",
        },
        Entry {
            path: long.clone(),
            mode: 0o644,
            bytes: &[1; 700],
        },
    ];
    let archive = dir.join("out.tar");
    std::fs::write(&archive, write(&entries).unwrap()).unwrap();
    let listed = Command::new("tar")
        .arg("-tvf")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(
        listed.contains("-rwxr-xr-x") && listed.contains(".adds/simple-books/reader.sh"),
        "{listed}"
    );
    assert!(listed.contains(&long), "{listed}");
    let unpacked = dir.join("unpacked");
    std::fs::create_dir_all(&unpacked).unwrap();
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read(unpacked.join(&long)).unwrap(), vec![1; 700]);
}

#[test]
fn what_gnu_tar_writes_as_ustar_reads() {
    if !has_tar() {
        return;
    }
    let dir = scratch("gnu");
    let files = dir.join("files");
    std::fs::create_dir_all(files.join(".kobo")).unwrap();
    std::fs::write(files.join(".kobo/Kobo.tgz"), [9u8; 1500]).unwrap();
    std::fs::write(files.join("top"), b"t").unwrap();
    let archive = dir.join("gnu.tar");
    let status = Command::new("tar")
        .args(["--format=ustar", "--sort=name", "-cf"])
        .arg(&archive)
        .arg("-C")
        .arg(&files)
        .args([".kobo", "top"])
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&archive).unwrap();
    let found: Vec<(String, usize)> = read(&bytes)
        .unwrap()
        .into_iter()
        .map(|e| (e.path, e.bytes.len()))
        .collect();
    assert_eq!(
        found,
        vec![(".kobo/Kobo.tgz".to_owned(), 1500), ("top".to_owned(), 1)]
    );
}
