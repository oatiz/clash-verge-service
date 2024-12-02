use super::data::*;
use anyhow::{bail, Context, Result};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct ClashStatus {
    pub info: Option<StartBody>,
    pub process_id: Option<u32>,
    pub is_running: Arc<AtomicBool>,
}

impl ClashStatus {
    pub fn global() -> &'static Arc<Mutex<ClashStatus>> {
        static CLASHSTATUS: OnceCell<Arc<Mutex<ClashStatus>>> = OnceCell::new();

        CLASHSTATUS.get_or_init(|| Arc::new(Mutex::new(ClashStatus::default())))
    }
}

/// GET /version
/// 获取服务进程的版本
pub fn get_version() -> Result<HashMap<String, String>> {
    let version = env!("CARGO_PKG_VERSION");

    let mut map = HashMap::new();

    map.insert("service".into(), "Clash Verge Service".into());
    map.insert("version".into(), version.into());

    Ok(map)
}

/// POST /start_clash
/// 启动clash进程
pub fn start_clash(body: StartBody) -> Result<()> {
    let body_cloned = body.clone();
    let config_dir = body.config_dir.as_str();
    let config_file = body.config_file.as_str();
    let args = vec!["-d", config_dir, "-f", config_file];

    let log = File::create(&body.log_file).context("failed to open log")?;

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;

        unsafe {
            let child = Command::new(&body.bin_path)
                .args(&args)
                .stdout(log)
                .pre_exec(|| {
                    // 设置新的进程组
                    libc::setpgid(0, 0);
                    Ok(())
                })
                .spawn()?;

            let pid = child.id();
            let mut arc = ClashStatus::global().lock();
            arc.info = Some(body_cloned);
            arc.process_id = Some(pid);
            arc.is_running.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let child = Command::new(&body.bin_path)
            .args(&args)
            .stdout(log)
            .spawn()?;

        let pid = child.id();
        let mut arc = ClashStatus::global().lock();
        arc.info = Some(body_cloned);
        arc.process_id = Some(pid);
        arc.is_running.store(true, Ordering::SeqCst);
    }

    Ok(())
}

/// POST /stop_clash
/// 停止clash进程
pub fn stop_clash() -> Result<()> {
    let mut arc = ClashStatus::global().lock();
    let pid = arc.process_id.take();
    arc.info = None;

    #[cfg(target_os = "linux")]
    if let Some(pid) = pid {
        unsafe {
            // 1. 发送 SIGTERM
            libc::kill(pid as i32, libc::SIGTERM);

            // 2. 等待进程退出（最多1秒）
            let mut attempts = 0;
            while attempts < 10 {
                match libc::kill(pid as i32, 0) {
                    0 => {
                        // 进程还在运行
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        attempts += 1;
                    }
                    _ => {
                        // 进程已经退出
                        return Ok(());
                    }
                }
            }

            // 3. 如果还没退出，发送 SIGKILL
            libc::kill(pid as i32, libc::SIGKILL);
            // 等待一小段时间确保进程被终止
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[cfg(not(target_os = "linux"))]
    if let Some(pid) = pid {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
        let mut system = System::new_with_specifics(
            RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
        );
        system.refresh_processes(ProcessesToUpdate::All);

        if let Some(process) = system.process(sysinfo::Pid::from_u32(pid)) {
            process.kill();
        }
    }

    Ok(())
}

/// GET /get_clash
/// 获取clash当前执行信息
pub fn get_clash() -> Result<StartBody> {
    let arc = ClashStatus::global().lock();

    match arc.info.clone() {
        Some(info) => Ok(info),
        None => bail!("clash not executed"),
    }
}

// 在服务启动时调用这个函数
#[cfg(target_os = "linux")]
pub fn init_signal_handler() {
    use nix::sys::signal::{self, SigAction, SigHandler, Signal};
    unsafe {
        // 设置 SIGCHLD 处理函数，自动回收子进程
        let sa = SigAction::new(
            SigHandler::Handler(handle_sigchld),
            signal::SaFlags::empty(),
            signal::SigSet::empty(),
        );
        let _ = signal::sigaction(Signal::SIGCHLD, &sa);
    }
}

#[cfg(target_os = "linux")]
extern "C" fn handle_sigchld(_: i32) {
    unsafe {
        // 循环等待所有已终止的子进程
        while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {}
    }
}
