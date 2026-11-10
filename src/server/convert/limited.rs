//! The re-exec helper. A new process applies limits to itself and then becomes
//! kepubify, which means no `unsafe` `pre_exec`, and nothing here runs in the
//! server's own process.
//!
//! kepubify is a trusted program handed untrusted input. These limits are for the
//! day it has a bug: it can read the book and write its own scratch directory, and
//! that's about all.
//!
//! The same limits, with no file access at all, hold the reader-format
//! conversion, which runs in a child of this program: see [`confine`].

use std::{
    ffi::OsString,
    io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

use landlock::{
    ABI, Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetError,
    RulesetStatus, path_beneath_rules,
};
use rustix::process::{Resource, Rlimit, setrlimit};

const CPU_SECONDS: u64 = 300;
const MAX_FILE_BYTES: u64 = 2048 * 1024 * 1024;
const MAX_OPEN_FILES: u64 = 256;

/// The arguments of the hidden `exec-limited` command.
#[derive(Debug, Clone)]
pub struct Limits {
    pub program: PathBuf,
    /// Read-only access.
    pub read: Vec<PathBuf>,
    /// The only place it can write, and its working directory.
    pub work: PathBuf,
    pub args: Vec<OsString>,
}

fn limit(resource: Resource, value: u64) -> io::Result<()> {
    setrlimit(
        resource,
        Rlimit {
            current: Some(value),
            maximum: Some(value),
        },
    )?;
    Ok(())
}

/// Best effort: a kernel without Landlock still has everything else here, and the
/// outcome goes in the log.
fn restrict_files(read: &[&Path], write: &[&Path]) -> Result<RulesetStatus, RulesetError> {
    let abi = ABI::V4;
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))?
        .handle_access(AccessNet::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules(read, AccessFs::from_read(abi)))?
        .add_rules(path_beneath_rules(write, AccessFs::from_all(abi)))?
        .restrict_self()?;
    Ok(status.ruleset)
}

/// Limits this process: CPU time, file sizes, open files, no new privileges,
/// optionally its address space, and files and network to only `read` and
/// `write`. Files already open stay usable.
///
/// # Errors
///
/// If a resource limit can't be set. Landlock is best effort, and only noted
/// on stderr when it can't be applied.
pub fn confine(read: &[&Path], write: &[&Path], memory: Option<u64>) -> io::Result<()> {
    limit(Resource::Core, 0)?;
    limit(Resource::Cpu, CPU_SECONDS)?;
    limit(Resource::Fsize, MAX_FILE_BYTES)?;
    limit(Resource::Nofile, MAX_OPEN_FILES)?;
    if let Some(bytes) = memory {
        limit(Resource::As, bytes)?;
    }
    rustix::thread::set_no_new_privs(true)?;

    match restrict_files(read, write) {
        Ok(RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced) => {}
        Ok(RulesetStatus::NotEnforced) => {
            eprintln!("Landlock isn't available here; running without it");
        }
        Err(error) => eprintln!("couldn't apply Landlock: {error}"),
    }
    Ok(())
}

/// Applies the limits and becomes the program. Only returns on failure.
pub fn exec(limits: &Limits) -> io::Error {
    let mut read: Vec<&Path> = limits.read.iter().map(PathBuf::as_path).collect();
    read.push(&limits.program);
    if let Err(error) = confine(&read, &[limits.work.as_path()], None) {
        return error;
    }
    Command::new(&limits.program)
        .args(&limits.args)
        .current_dir(&limits.work)
        .exec()
}
