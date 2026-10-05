//! Verify process ownership across daemon restart and PID reuse.
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started: String,
    pub boot: String,
}

impl ProcessIdentity {
    pub fn read(pid: u32) -> Result<Self> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields: Vec<_> = stat
            .rsplit_once(") ")
            .ok_or_else(|| anyhow::anyhow!("invalid process stat"))?
            .1
            .split_whitespace()
            .collect();
        Ok(Self {
            pid,
            boot: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?,
            started: fields
                .get(19)
                .ok_or_else(|| anyhow::anyhow!("missing process start time"))?
                .to_string(),
        })
    }
    pub fn alive(&self) -> Result<bool> {
        match Self::read(self.pid) {
            Ok(current) => Ok(current.started == self.started && current.boot == self.boot),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}
