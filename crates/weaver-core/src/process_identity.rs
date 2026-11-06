//! Verify process ownership across daemon restart and PID reuse.
//!
//! Linux reads these facts from `/proc`; macOS, which has no `/proc`, asks the
//! kernel through libproc and sysctl.
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started: String,
    pub boot: String,
}

impl ProcessIdentity {
    /// The identity of process `pid`. A process that does not exist is an
    /// [`std::io::ErrorKind::NotFound`] error on every platform.
    pub fn read(pid: u32) -> Result<Self> {
        Ok(Self {
            pid,
            started: platform::start_time(pid)?,
            boot: boot_id()?,
        })
    }
    pub fn alive(&self) -> Result<bool> {
        match Self::read(self.pid) {
            Ok(current) => Ok(current.started == self.started && current.boot == self.boot),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// An identifier of the current boot, so an identity recorded before a reboot
/// never matches a process after it.
pub fn boot_id() -> Result<String> {
    platform::boot_id()
}

/// Whether process `pid` exists and has not exited. A zombie, which has exited
/// and only waits to be reaped, is not running.
pub fn running(pid: u32) -> Result<bool> {
    match platform::running(pid) {
        Err(error) if is_not_found(&error) => Ok(false),
        result => result,
    }
}

/// Whether any member of process group `pgid` is still running.
pub fn group_alive(pgid: u32) -> Result<bool> {
    platform::group_alive(pgid)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::Result;

    /// The fields of a `/proc/<pid>/stat` line after the command name, which is
    /// in parentheses and may itself hold spaces: the state first, then the
    /// parent pid and the process group.
    fn stat_fields(stat: &str) -> Option<Vec<&str>> {
        stat.rsplit_once(") ")
            .map(|(_, fields)| fields.split_whitespace().collect())
    }

    /// The process's start time in clock ticks since boot.
    pub(super) fn start_time(pid: u32) -> Result<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields = stat_fields(&stat).ok_or_else(|| anyhow::anyhow!("invalid process stat"))?;
        Ok(fields
            .get(19)
            .ok_or_else(|| anyhow::anyhow!("missing process start time"))?
            .to_string())
    }

    pub(super) fn boot_id() -> Result<String> {
        Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?)
    }

    pub(super) fn running(pid: u32) -> Result<bool> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        Ok(stat_fields(&stat).and_then(|fields| fields.first().copied()) != Some("Z"))
    }

    pub(super) fn group_alive(pgid: u32) -> Result<bool> {
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
                continue;
            }
            let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let Some(fields) = stat_fields(&stat) else {
                continue;
            };
            if fields.first().copied() != Some("Z")
                && fields
                    .get(2)
                    .is_some_and(|group| group.parse::<u32>() == Ok(pgid))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use anyhow::Result;
    use std::io;

    // A process group rarely holds more members than this; a larger one grows the buffer.
    const GROUP_PIDS: usize = 64;

    fn bsd_info(pid: u32) -> Result<libc::proc_bsdinfo> {
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: proc_bsdinfo is plain old data, for which all zeroes is valid.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: the buffer is one proc_bsdinfo, and `size` is its size in bytes.
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
        if written == size {
            return Ok(info);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            // The error kind a missing /proc entry gives on Linux, which callers match.
            return Err(io::Error::from(io::ErrorKind::NotFound).into());
        }
        Err(anyhow::anyhow!("reading process {pid}: {error}"))
    }

    /// The process's start time, as seconds and microseconds since the epoch.
    pub(super) fn start_time(pid: u32) -> Result<String> {
        let info = bsd_info(pid)?;
        Ok(format!(
            "{}.{:06}",
            info.pbi_start_tvsec, info.pbi_start_tvusec
        ))
    }

    /// The kernel's UUID for this boot session.
    pub(super) fn boot_id() -> Result<String> {
        let mut buffer = [0u8; 64];
        let mut length = buffer.len();
        // SAFETY: the name is NUL-terminated, and `length` is the buffer's size in bytes.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(std::str::from_utf8(&buffer[..length])?
            .trim_end_matches('\0')
            .to_string())
    }

    pub(super) fn group_alive(pgid: u32) -> Result<bool> {
        let mut pids: Vec<libc::pid_t> = vec![0; GROUP_PIDS];
        loop {
            let size = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
            // SAFETY: the buffer holds pids.len() pids, and `size` is its size in bytes.
            let count = unsafe {
                libc::proc_listpgrppids(pgid as libc::pid_t, pids.as_mut_ptr().cast(), size)
            };
            if count < 0 {
                return Err(io::Error::last_os_error().into());
            }
            let count = count as usize;
            if count < pids.len() {
                pids.truncate(count);
                break;
            }
            pids.resize(pids.len() * 2, 0);
        }
        for pid in pids {
            if super::running(pid as u32)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn running(pid: u32) -> Result<bool> {
        Ok(bsd_info(pid)?.pbi_status != libc::SZOMB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    fn sleeper_in_own_group() -> Child {
        Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap()
    }

    #[test]
    fn an_identity_matches_its_process_only_while_the_process_exists() {
        let mut child = sleeper_in_own_group();
        let identity = ProcessIdentity::read(child.id()).unwrap();
        assert!(identity.alive().unwrap());

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!identity.alive().unwrap());
    }

    #[test]
    fn a_group_stops_being_alive_when_its_members_exit_before_they_are_reaped() {
        // Regression: stop() waits for a killed script group, whose leader stays a
        // zombie until the daemon reaps it, so a zombie must not count as alive.
        let mut child = sleeper_in_own_group();
        let pgid = child.id();
        assert!(group_alive(pgid).unwrap());

        child.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while group_alive(pgid).unwrap() {
            assert!(Instant::now() < deadline, "a killed group stayed alive");
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait().unwrap();
    }
}
