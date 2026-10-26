//! Confining a render that runs Lua filters (SPEC §5.6). An approved filter is code somebody
//! reviewed, and still code: it runs with no network, writing nowhere but the render's own
//! scratch directory, and reading only what a render needs — the system's programs and
//! libraries, the Quarto install, and the render's project. Not a server's database, not its
//! environment, not the user's home.
//!
//! - **Linux**: Landlock (files, TCP, signals, abstract sockets) and a seccomp filter that refuses
//!   every new socket and io_uring. Both are put on the thread that starts the render, and so
//!   on everything that starts; neither needs a privilege, so they work inside a container under
//!   Docker's default seccomp profile, where bubblewrap's user namespaces do not.
//! - **macOS**: `sandbox-exec` with a profile that denies the network and every write outside
//!   the scratch directory. Reads are not narrowed: a filter can read what the user can, though
//!   it has nowhere to send it but into the render the user is looking at.
//! - **Elsewhere** (Windows) nothing confines a render, and [`confinement`] says so.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where a confined render may go: `work` is the one place it may write (its project, its
/// temporary files, its caches), `read` what it may read besides.
#[derive(Debug, Clone)]
pub(crate) struct Jail {
    pub work: PathBuf,
    pub read: Vec<PathBuf>,
}

/// What this machine can do to confine a render: `Ok` with a word on how, or why it cannot.
/// Asked once, then remembered.
pub fn confinement() -> Result<&'static str, String> {
    static PROBED: std::sync::OnceLock<Result<&'static str, String>> = std::sync::OnceLock::new();
    PROBED.get_or_init(probe).clone()
}

/// The command to run `bin` confined in `jail`, before [`run`] runs it.
pub(crate) fn command(bin: &Path, jail: &Jail) -> Command {
    imp::command(bin, jail)
}

/// Run `cmd` (from [`command`]) confined in `jail`, killed with what it started after `timeout`
/// (see [`crate::pandoc::run_bounded`]).
pub(crate) fn run(
    cmd: &mut Command,
    jail: &Jail,
    timeout: Duration,
) -> std::io::Result<Option<std::process::Output>> {
    imp::run(cmd, jail, timeout)
}

fn probe() -> Result<&'static str, String> {
    imp::probe()
}

/// The folders every program needs to read to start at all, wherever they are on this system.
#[cfg(target_os = "linux")]
const SYSTEM: &[&str] = &[
    "/usr", "/lib", "/lib32", "/lib64", "/libx32", "/bin", "/sbin", "/etc", "/opt", "/nix", "/gnu", "/proc",
];

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use landlock::{
        ABI, Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
        path_beneath_rules,
    };

    /// The newest Landlock this code knows how to ask for; an older kernel enforces what it has.
    const ABI_WANTED: ABI = ABI::V6;

    pub fn command(bin: &Path, _jail: &Jail) -> Command {
        Command::new(bin)
    }

    pub fn run(
        cmd: &mut Command,
        jail: &Jail,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::Output>> {
        // A thread of its own, confined and then done with: Landlock and seccomp hold for the
        // thread that asks and what it starts, and this thread starts only the render.
        std::thread::scope(|s| {
            s.spawn(|| {
                confine(jail).map_err(std::io::Error::other)?;
                crate::pandoc::run_bounded(cmd, None, timeout)
            })
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("the render's thread panicked")))
        })
    }

    pub fn probe() -> Result<&'static str, String> {
        let dir = crate::pandoc::tempdir().map_err(|e| e.to_string())?;
        let jail = Jail { work: dir.to_path_buf(), read: Vec::new() };
        let probed = std::thread::spawn(move || confine(&jail)).join();
        match probed {
            Ok(Ok(())) => Ok("Landlock and seccomp"),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("probing the sandbox panicked".into()),
        }
    }

    /// Confine the calling thread (and whatever it starts) to `jail`. Fails rather than leaving
    /// the thread less confined than it says: no Landlock at all, or no seccomp, is an error.
    pub(super) fn confine(jail: &Jail) -> Result<(), String> {
        let abi = ABI_WANTED;
        let err = |e: landlock::RulesetError| format!("Landlock: {e}");
        let mut read: Vec<PathBuf> = SYSTEM.iter().map(PathBuf::from).collect();
        read.extend(jail.read.iter().cloned());
        let devices = ["/dev/null", "/dev/zero", "/dev/random", "/dev/urandom", "/dev/full"];
        let status = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(err)?
            .handle_access(AccessNet::from_all(abi))
            .map_err(err)?
            .scope(Scope::from_all(abi))
            .map_err(err)?
            .create()
            .map_err(err)?
            .add_rules(path_beneath_rules(&read, AccessFs::from_read(abi)))
            .map_err(err)?
            .add_rules(path_beneath_rules(devices, AccessFs::from_read(abi) | AccessFs::WriteFile))
            .map_err(err)?
            .add_rules(path_beneath_rules([&jail.work], AccessFs::from_all(abi)))
            .map_err(err)?
            .restrict_self()
            .map_err(err)?;
        if status.ruleset == RulesetStatus::NotEnforced {
            return Err("this kernel does not enforce Landlock (it needs Linux 5.13 or later, with \
                        `landlock` among the LSMs it boots with)"
                .into());
        }
        if !status.no_new_privs {
            return Err("could not set no_new_privs".into());
        }
        no_sockets()
    }

    // A seccomp filter, by hand: a dozen instructions do not need a crate.
    #[repr(C)]
    struct SockFilter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }

    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }

    const LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const JGE_K: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
    const RET_K: u16 = 0x06; // BPF_RET | BPF_K
    const RET_ALLOW: u32 = 0x7fff_0000;
    const RET_KILL_PROCESS: u32 = 0x8000_0000;
    const RET_ERRNO: u32 = 0x0005_0000;
    // `struct seccomp_data`: the syscall number, then the architecture.
    const NR: u32 = 0;
    const ARCH: u32 = 4;
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;

    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7;

    /// Refuse `socket(2)` — every family: a render talks to nothing — and `io_uring_setup(2)`,
    /// which could open one all the same. A process of another architecture (x86's 32-bit
    /// calls, which number their syscalls differently) is killed rather than checked.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn no_sockets() -> Result<(), String> {
        let ins = |code, jt, jf, k| SockFilter { code, jt, jf, k };
        let deny = RET_ERRNO | libc::EACCES as u32;
        let filter = [
            ins(LD_W_ABS, 0, 0, ARCH),
            ins(JEQ_K, 1, 0, AUDIT_ARCH),
            ins(RET_K, 0, 0, RET_KILL_PROCESS),
            ins(LD_W_ABS, 0, 0, NR),
            // x32 numbers its calls from bit 30 up, under the same architecture.
            ins(JGE_K, 3, 0, 0x4000_0000),
            ins(JEQ_K, 2, 0, libc::SYS_socket as u32),
            ins(JEQ_K, 1, 0, libc::SYS_io_uring_setup as u32),
            ins(RET_K, 0, 0, RET_ALLOW),
            ins(RET_K, 0, 0, deny),
        ];
        let prog = SockFprog { len: filter.len() as u16, filter: filter.as_ptr() };
        // SAFETY: prctl(2) with a pointer to a filter that outlives the call; the kernel copies
        // it. no_new_privs is already set (`restrict_self`), as an unprivileged filter needs.
        let r = unsafe {
            libc::prctl(libc::PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog as *const SockFprog, 0, 0)
        };
        if r != 0 {
            return Err(format!("seccomp: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    fn no_sockets() -> Result<(), String> {
        Err("no seccomp filter for this architecture".into())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    pub fn command(bin: &Path, jail: &Jail) -> Command {
        let mut cmd = Command::new(SANDBOX_EXEC);
        cmd.arg("-p").arg(profile(jail)).arg(bin);
        cmd
    }

    pub fn run(
        cmd: &mut Command,
        _jail: &Jail,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::Output>> {
        crate::pandoc::run_bounded(cmd, None, timeout)
    }

    pub fn probe() -> Result<&'static str, String> {
        let ok = Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
        if ok { Ok("sandbox-exec") } else { Err(format!("{SANDBOX_EXEC} is not there, or does not work")) }
    }

    /// No network, and no writes but to the scratch directory and the null device. The path
    /// is the real one: `/var/folders/…` is a link to `/private/var/folders/…`, and a profile
    /// matches what the kernel sees.
    fn profile(jail: &Jail) -> String {
        let work = std::fs::canonicalize(&jail.work).unwrap_or_else(|_| jail.work.clone());
        let quoted = format!("{:?}", work.to_string_lossy());
        format!(
            "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n\
             (allow file-write* (subpath {quoted}) (literal \"/dev/null\") (literal \"/dev/zero\"))\n"
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    pub fn command(bin: &Path, _jail: &Jail) -> Command {
        Command::new(bin)
    }

    pub fn run(
        _cmd: &mut Command,
        _jail: &Jail,
        _timeout: Duration,
    ) -> std::io::Result<Option<std::process::Output>> {
        Err(std::io::Error::other("nothing confines a render on this system"))
    }

    pub fn probe() -> Result<&'static str, String> {
        Err("nothing confines a render on this system".into())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A thread confined to a scratch directory can read the system but not the rest of the
    /// machine, write only to its scratch directory, open no socket, and not read another
    /// process's environment.
    #[test]
    fn a_confined_thread_reaches_only_its_jail() {
        if let Err(why) = confinement() {
            eprintln!("skipped: {why}");
            return;
        }
        let outside = crate::pandoc::tempdir().unwrap();
        std::fs::write(outside.join("secret"), "x").unwrap();
        let work = crate::pandoc::tempdir().unwrap();
        // Something else of ours, started before: its environment is not to be read.
        let mut other = Command::new("sleep").arg("30").env("LEMMATE_TEST_MARK", "sekrit").spawn().unwrap();
        let environ = format!("/proc/{}/environ", other.id());
        assert!(std::fs::read(&environ).is_ok(), "readable before confinement");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let jail = Jail { work: work.to_path_buf(), read: Vec::new() };
        let (outside2, work2) = (outside.to_path_buf(), work.to_path_buf());
        let got = std::thread::spawn(move || {
            confine_here(&jail);
            (
                std::fs::read("/etc/hostname").is_ok() || std::fs::read("/etc/os-release").is_ok(),
                std::fs::read(outside2.join("secret")).is_err(),
                std::fs::write(outside2.join("new"), "x").is_err(),
                std::fs::write(work2.join("new"), "x").is_ok(),
                std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
                std::net::UdpSocket::bind("127.0.0.1:0").is_err(),
                std::fs::read(&environ).is_err(),
            )
        })
        .join()
        .unwrap();
        let _ = other.kill();
        let _ = other.wait();
        assert_eq!(got, (true, true, true, true, true, true, true));
        // Only that thread: this one is as it was.
        assert!(std::fs::write(outside.join("after"), "x").is_ok());
    }

    /// Runs a program confined: the jail holds for what the thread starts, and its children.
    #[test]
    fn what_the_thread_starts_is_confined_too() {
        if let Err(why) = confinement() {
            eprintln!("skipped: {why}");
            return;
        }
        let outside = crate::pandoc::tempdir().unwrap();
        let work = crate::pandoc::tempdir().unwrap();
        let jail = Jail { work: work.to_path_buf(), read: Vec::new() };
        let script = format!(
            "echo in > {w}/a; echo out > {o}/b; sh -c 'echo deeper > {o}/c'; cat {o}/../nothing; exit 0",
            w = work.display(),
            o = outside.display()
        );
        let mut cmd = command(Path::new("/bin/sh"), &jail);
        cmd.arg("-c").arg(script);
        let out = run(&mut cmd, &jail, Duration::from_secs(10)).unwrap().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(work.join("a").is_file(), "it writes its own directory");
        assert!(!outside.join("b").exists() && !outside.join("c").exists(), "and nothing else");
    }

    fn confine_here(jail: &Jail) {
        imp::confine(jail).unwrap();
    }
}
