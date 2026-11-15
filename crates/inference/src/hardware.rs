//! Hardware detection used for model recommendations and diagnostics.

use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Accel {
    /// Apple GPU via Metal; memory is unified with the CPU.
    Metal,
    Cuda {
        name: String,
        vram: Option<u64>,
    },
    Cpu,
}

impl Accel {
    pub fn label(&self) -> String {
        match self {
            Accel::Metal => "Metal".into(),
            Accel::Cuda { name, .. } => format!("CUDA ({name})"),
            Accel::Cpu => "CPU".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hardware {
    pub os: String,
    pub os_version: Option<String>,
    pub arch: String,
    pub cpu_cores: usize,
    pub total_mem: u64,
    pub available_mem: u64,
    pub accel: Accel,
}

impl Hardware {
    /// Memory budget for model weights + KV cache. With unified memory this
    /// is system RAM; with a discrete GPU it's VRAM (layers beyond it spill to
    /// the CPU, which works but is slow).
    pub fn model_memory(&self) -> u64 {
        match &self.accel {
            Accel::Cuda { vram: Some(v), .. } => *v,
            _ => self.total_mem,
        }
    }
}

pub fn detect() -> Hardware {
    let (total_mem, available_mem) = match sysinfo::System::new() {
        Ok(mut sys) => {
            sys.refresh_memory();
            (sys.total_memory(), sys.available_memory())
        }
        Err(_) => (0, 0),
    };
    let accel = detect_accel();
    Hardware {
        os: std::env::consts::OS.to_string(),
        os_version: sysinfo::System::long_os_version().ok(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        total_mem,
        available_mem,
        accel,
    }
}

fn detect_accel() -> Accel {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Accel::Metal;
    }
    if let Some(cuda) = nvidia_smi() {
        return cuda;
    }
    Accel::Cpu
}

/// Query `nvidia-smi` with a short timeout. Absence simply means "no CUDA".
fn nvidia_smi() -> Option<Accel> {
    let mut child = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let (name, mem) = line.split_once(',')?;
    Some(Accel::Cuda { name: name.trim().to_string(), vram: mem.trim().parse::<u64>().ok().map(|mib| mib * 1024 * 1024) })
}

/// Free space on the volume containing `path` (or its nearest existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let p = std::fs::canonicalize(p).ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks.iter().filter(|d| p.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len()).map(|d| d.available_space())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_something_sane() {
        let h = detect();
        assert!(h.total_mem > 0);
        assert!(h.cpu_cores >= 1);
        assert!(h.model_memory() > 0);
        assert!(free_space(&std::env::temp_dir()).is_some());
    }
}
