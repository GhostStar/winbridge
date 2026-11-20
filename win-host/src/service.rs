use crate::config::{CaptureMode, Codec, Config};
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::c_void,
    io, mem, ptr,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        OnceLock,
    },
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_EXISTS, HANDLE, NO_ERROR, WAIT_OBJECT_0},
    Security::{
        DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TokenPrimary,
        TokenSessionId, TOKEN_ALL_ACCESS,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        RemoteDesktop::WTSGetActiveConsoleSessionId,
        Services::*,
        Threading::{
            CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, TerminateProcess,
            WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
        },
    },
};

const SERVICE_NAME: &str = "WinBridgeHost";
const SERVICE_DISPLAY_NAME: &str = "WinBridge Remote Window Host";

static SERVICE_CONFIG: OnceLock<Config> = OnceLock::new();
static STATUS_HANDLE: OnceLock<usize> = OnceLock::new();
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static SESSION_BUMP: AtomicU32 = AtomicU32::new(0);

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn quote_arg(s: &str) -> String {
    if s.is_empty() || s.contains([' ', '\t', '"']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_owned()
    }
}

fn codec_arg(c: Codec) -> &'static str {
    match c {
        Codec::Auto => "auto",
        Codec::H264 => "h264",
        Codec::Jpeg => "jpeg",
        Codec::Raw => "raw",
    }
}

fn capture_mode_arg(m: CaptureMode) -> &'static str {
    match m {
        CaptureMode::Window => "window",
        CaptureMode::MonitorCrop => "monitor-crop",
    }
}

fn forwarded_args(config: &Config) -> Vec<String> {
    let mut args = vec![
        "--bind".into(),
        config.bind.to_string(),
        "--port".into(),
        config.port.to_string(),
        "--fps".into(),
        config.fps.to_string(),
        "--bitrate".into(),
        config.bitrate.to_string(),
        "--codec".into(),
        codec_arg(config.codec).into(),
        "--capture-mode".into(),
        capture_mode_arg(config.capture_mode).into(),
        "--log-level".into(),
        config.log_level.clone(),
    ];
    if !config.token.is_empty() {
        args.push("--token".into());
        args.push(config.token.clone());
    }
    if config.capture_cursor {
        args.push("--capture-cursor".into());
    }
    args
}

fn build_command_line(exe: &str, mode_flag: &str, config: &Config) -> String {
    let mut parts = vec![quote_arg(exe), mode_flag.to_owned()];
    for arg in forwarded_args(config) {
        parts.push(quote_arg(&arg));
    }
    parts.join(" ")
}

fn wait_for_stopped(svc: SC_HANDLE) {
    let mut status: SERVICE_STATUS = unsafe { mem::zeroed() };
    for _ in 0..35 {
        if unsafe { QueryServiceStatus(svc, &mut status) } == 0
            || status.dwCurrentState == SERVICE_STOPPED
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Installs (or updates) `WinBridgeHost` as an auto-start LocalSystem Windows Service
/// and starts it immediately. Must be run from an elevated Administrator PowerShell.
pub fn install(config: &Config) -> Result<()> {
    let exe = std::env::current_exe()
        .context("locate current win-host.exe")?
        .to_string_lossy()
        .into_owned();
    let bin_path = build_command_line(&exe, "--service", config);
    let svc_name = to_wide(SERVICE_NAME);
    let display_name = to_wide(SERVICE_DISPLAY_NAME);
    let bin_wide = to_wide(&bin_path);

    unsafe {
        let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            bail!(
                "OpenSCManagerW failed ({}). 请使用“以管理员身份运行”的 PowerShell 执行 --install-service",
                io::Error::last_os_error()
            );
        }

        let mut svc = CreateServiceW(
            scm,
            svc_name.as_ptr(),
            display_name.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            bin_wide.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(), // LocalSystem
            ptr::null(),
        );

        if svc.is_null() {
            if GetLastError() == ERROR_SERVICE_EXISTS {
                svc = OpenServiceW(scm, svc_name.as_ptr(), SERVICE_ALL_ACCESS);
                if svc.is_null() {
                    let err = io::Error::last_os_error();
                    CloseServiceHandle(scm);
                    bail!("OpenServiceW failed: {err}");
                }
                // Stop existing service before updating its command line so new config takes effect.
                let mut status: SERVICE_STATUS = mem::zeroed();
                let _ = ControlService(svc, SERVICE_CONTROL_STOP, &mut status);
                wait_for_stopped(svc);
                if ChangeServiceConfigW(
                    svc,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bin_wide.as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    display_name.as_ptr(),
                ) == 0
                {
                    let err = io::Error::last_os_error();
                    CloseServiceHandle(svc);
                    CloseServiceHandle(scm);
                    bail!("ChangeServiceConfigW failed: {err}");
                }
                println!("Updated existing Windows Service '{SERVICE_NAME}'.");
            } else {
                let err = io::Error::last_os_error();
                CloseServiceHandle(scm);
                bail!("CreateServiceW failed: {err}");
            }
        } else {
            println!("Installed Windows Service '{SERVICE_NAME}' (LocalSystem, auto-start).");
        }

        if StartServiceW(svc, 0, ptr::null()) == 0 {
            let code = GetLastError();
            if code != ERROR_SERVICE_ALREADY_RUNNING {
                let err = io::Error::last_os_error();
                CloseServiceHandle(svc);
                CloseServiceHandle(scm);
                bail!("StartServiceW failed: {err}");
            }
        }

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }

    println!("Service '{SERVICE_NAME}' is running on {}:{} (UAC / Secure Desktop enabled).", config.bind, config.port);
    println!("Command line: {bin_path}");
    Ok(())
}

/// Stops and removes the `WinBridgeHost` Windows Service.
pub fn uninstall() -> Result<()> {
    let svc_name = to_wide(SERVICE_NAME);
    unsafe {
        let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            bail!(
                "OpenSCManagerW failed ({}). 请使用管理员 PowerShell 执行 --uninstall-service",
                io::Error::last_os_error()
            );
        }
        let svc = OpenServiceW(scm, svc_name.as_ptr(), SERVICE_ALL_ACCESS);
        if svc.is_null() {
            let err = io::Error::last_os_error();
            CloseServiceHandle(scm);
            bail!("OpenServiceW('{SERVICE_NAME}') failed: {err}");
        }
        let mut status: SERVICE_STATUS = mem::zeroed();
        let _ = ControlService(svc, SERVICE_CONTROL_STOP, &mut status);
        wait_for_stopped(svc);
        if DeleteService(svc) == 0 {
            let err = io::Error::last_os_error();
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
            bail!("DeleteService failed: {err}");
        }
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }
    println!("Stopped and uninstalled Windows Service '{SERVICE_NAME}'.");
    Ok(())
}

/// Entrypoint invoked by Windows SCM when started with `--service`.
pub fn run_service_dispatcher(config: Config) -> Result<()> {
    let _ = SERVICE_CONFIG.set(config);
    let mut name = to_wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        bail!(
            "StartServiceCtrlDispatcherW failed ({}). --service 仅由 Windows 服务管理器启动，安装请用 --install-service",
            io::Error::last_os_error()
        );
    }
    Ok(())
}

fn report_status(state: u32, controls: u32, exit_code: u32) {
    let Some(&raw_handle) = STATUS_HANDLE.get() else {
        return;
    };
    let handle = raw_handle as SERVICE_STATUS_HANDLE;
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: controls,
        dwWin32ExitCode: exit_code,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 3000,
    };
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

unsafe extern "system" fn service_ctrl_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP_REQUESTED.store(true, Ordering::Release);
            report_status(SERVICE_STOP_PENDING, 0, 0);
            NO_ERROR
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            SESSION_BUMP.fetch_add(1, Ordering::AcqRel);
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => 120, // ERROR_CALL_NOT_IMPLEMENTED
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = to_wide(SERVICE_NAME);
    let handle = RegisterServiceCtrlHandlerExW(
        name.as_ptr(),
        Some(service_ctrl_handler),
        ptr::null(),
    );
    if handle.is_null() {
        return;
    }
    let _ = STATUS_HANDLE.set(handle as usize);
    report_status(SERVICE_START_PENDING, 0, 0);
    report_status(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE,
        0,
    );

    if let Some(config) = SERVICE_CONFIG.get() {
        supervisor_loop(config);
    }

    report_status(SERVICE_STOPPED, 0, 0);
}

struct ChildWorker {
    session_id: u32,
    process: HANDLE,
    job: HANDLE,
}

impl ChildWorker {
    fn is_alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 }
    }
}

impl Drop for ChildWorker {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            let _ = WaitForSingleObject(self.process, 2000);
            CloseHandle(self.process);
            if !self.job.is_null() {
                CloseHandle(self.job);
            }
        }
    }
}

fn spawn_session_worker(session_id: u32, config: &Config) -> Result<ChildWorker> {
    let exe = std::env::current_exe()?
        .to_string_lossy()
        .into_owned();
    let mut cmd = to_wide(&build_command_line(&exe, "--session-worker", config));
    let mut desktop = to_wide("winsta0\\default");

    unsafe {
        let mut sys_token: HANDLE = ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut sys_token) != 0,
            "OpenProcessToken failed: {}",
            io::Error::last_os_error()
        );
        let mut dup_token: HANDLE = ptr::null_mut();
        let dup_ok = DuplicateTokenEx(
            sys_token,
            TOKEN_ALL_ACCESS,
            ptr::null(),
            SecurityIdentification,
            TokenPrimary,
            &mut dup_token,
        ) != 0;
        CloseHandle(sys_token);
        ensure!(
            dup_ok && !dup_token.is_null(),
            "DuplicateTokenEx failed: {}",
            io::Error::last_os_error()
        );

        let sid = session_id;
        let set_ok = SetTokenInformation(
            dup_token,
            TokenSessionId,
            (&sid as *const u32).cast(),
            mem::size_of::<u32>() as u32,
        ) != 0;
        if !set_ok {
            let err = io::Error::last_os_error();
            CloseHandle(dup_token);
            bail!("SetTokenInformation(TokenSessionId={sid}) failed: {err}");
        }

        let mut si: STARTUPINFOW = mem::zeroed();
        si.cb = mem::size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        let mut pi: PROCESS_INFORMATION = mem::zeroed();

        let created = CreateProcessAsUserW(
            dup_token,
            ptr::null(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_NO_WINDOW,
            ptr::null(),
            ptr::null(),
            &si,
            &mut pi,
        ) != 0;
        CloseHandle(dup_token);
        ensure!(
            created,
            "CreateProcessAsUserW failed: {}",
            io::Error::last_os_error()
        );
        CloseHandle(pi.hThread);

        // Attach to a Job Object with KILL_ON_JOB_CLOSE so stopping the service always cleans up.
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if !job.is_null() {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let _ = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let _ = AssignProcessToJobObject(job, pi.hProcess);
        }

        Ok(ChildWorker {
            session_id,
            process: pi.hProcess,
            job,
        })
    }
}

fn supervisor_loop(config: &Config) {
    let mut worker: Option<ChildWorker> = None;
    let mut seen_bump = SESSION_BUMP.load(Ordering::Acquire);
    while !STOP_REQUESTED.load(Ordering::Acquire) {
        let bump = SESSION_BUMP.load(Ordering::Acquire);
        if bump != seen_bump {
            seen_bump = bump;
            tracing::info!("received Windows session-change notification");
        }
        let active_session = unsafe { WTSGetActiveConsoleSessionId() };
        if active_session == u32::MAX {
            worker = None;
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let need_spawn = match &worker {
            Some(w) if w.session_id == active_session && w.is_alive() => false,
            _ => true,
        };

        if need_spawn {
            // Drop old session worker before starting a new one so port 9876 is released cleanly.
            worker = None;
            std::thread::sleep(Duration::from_millis(200));
            match spawn_session_worker(active_session, config) {
                Ok(child) => {
                    tracing::info!(session_id = active_session, "spawned interactive session worker");
                    worker = Some(child);
                }
                Err(e) => {
                    tracing::error!(session_id = active_session, error = %e, "failed to spawn session worker");
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(500));
    }
    drop(worker);
}
