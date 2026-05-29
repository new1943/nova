
pub const SOCKET_PATH: &str = "/tmp/nova.sock";
pub const PID_FILE: &str = "/tmp/nova.pid";

pub fn write_pid_file() {
    let _ = std::fs::write(PID_FILE, std::process::id().to_string());
}

pub fn remove_pid_file() {
    let _ = std::fs::remove_file(PID_FILE);
}

pub fn check_pid_file() -> bool {
    if let Ok(pid_str) = std::fs::read_to_string(PID_FILE) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    }
}

pub struct PidGuard;

impl PidGuard {
    pub fn acquire() -> anyhow::Result<Self> {
        if check_pid_file() {
            anyhow::bail!("Daemon already running (PID file: {})", PID_FILE);
        }
        write_pid_file();
        Ok(Self)
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        remove_pid_file();
    }
}
