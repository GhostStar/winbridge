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
    Foundation::{
        CloseHandle, GetLastError, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_EXISTS, HANDLE,
        NO_ERROR, WAIT_OBJECT_0,
    },
    Security::{
        DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId,
        TokenUIAccess, TOKEN_ALL_ACCESS,
    },
    System::{
        Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken},
        Services::*,
        Threading::{
            CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, TerminateProcess,
            WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
            STARTUPINFOW,
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

struct ChildProcess {
    process: HANDLE,
}

impl ChildProcess {
    fn is_alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            let _ = WaitForSingleObject(self.process, 2000);
            CloseHandle(self.process);
        }
    }
}

struct SessionGroup {
    session_id: u32,
    user_mode: bool,
    worker: ChildProcess,
    helper: ChildProcess,
    job: HANDLE,
}

impl SessionGroup {
    fn is_alive(&self) -> bool {
        self.worker.is_alive() && self.helper.is_alive()
    }
}

impl Drop for SessionGroup {
    fn drop(&mut self) {
        unsafe {
            if !self.job.is_null() {
                CloseHandle(self.job);
            }
        }
    }
}

fn create_kill_on_close_job() -> HANDLE {
    unsafe {
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
        }
        job
    }
}

fn pick_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("allocate ephemeral loopback port for secure-helper")?;
    Ok(listener.local_addr()?.port())
}

fn has_logged_in_user(session_id: u32) -> bool {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if WTSQueryUserToken(session_id, &mut token) != 0 && !token.is_null() {
            CloseHandle(token);
            true
        } else {
            false
        }
    }
}

unsafe fn create_system_session_token(session_id: u32, ui_access: bool) -> Result<HANDLE> {
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
        SecurityImpersonation,
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

    if ui_access {
        let enabled: u32 = 1;
        let ok = SetTokenInformation(
            dup_token,
            TokenUIAccess,
            (&enabled as *const u32).cast(),
            mem::size_of::<u32>() as u32,
        ) != 0;
        if !ok {
            tracing::warn!(
                error = %io::Error::last_os_error(),
                "SetTokenInformation(TokenUIAccess=1) failed"
            );
        }
    }
    Ok(dup_token)
}

unsafe fn create_user_session_token(session_id: u32) -> Option<HANDLE> {
    let mut user_token: HANDLE = ptr::null_mut();
    if WTSQueryUserToken(session_id, &mut user_token) == 0 || user_token.is_null() {
        return None;
    }
    let mut primary: HANDLE = ptr::null_mut();
    let ok = DuplicateTokenEx(
        user_token,
        TOKEN_ALL_ACCESS,
        ptr::null(),
        SecurityImpersonation,
        TokenPrimary,
        &mut primary,
    ) != 0;
    CloseHandle(user_token);
    if ok && !primary.is_null() {
        Some(primary)
    } else {
        None
    }
}

unsafe fn spawn_with_token(
    token: HANDLE,
    cmd_line: &str,
    use_user_env: bool,
    job: HANDLE,
) -> Result<ChildProcess> {
    let mut cmd = to_wide(cmd_line);
    let mut desktop = to_wide("winsta0\\default");

    let mut env_block: *mut c_void = ptr::null_mut();
    let has_env = use_user_env
        && CreateEnvironmentBlock(&mut env_block, token, 0) != 0
        && !env_block.is_null();
    let flags = CREATE_NO_WINDOW | if has_env { CREATE_UNICODE_ENVIRONMENT } else { 0 };

    let mut si: STARTUPINFOW = mem::zeroed();
    si.cb = mem::size_of::<STARTUPINFOW>() as u32;
    si.lpDesktop = desktop.as_mut_ptr();
    let mut pi: PROCESS_INFORMATION = mem::zeroed();

    let created = CreateProcessAsUserW(
        token,
        ptr::null(),
        cmd.as_mut_ptr(),
        ptr::null(),
        ptr::null(),
        0,
        flags,
        if has_env { env_block as _ } else { ptr::null() },
        ptr::null(),
        &si,
        &mut pi,
    ) != 0;
    let err = if created {
        None
    } else {
        Some(io::Error::last_os_error())
    };
    if has_env {
        DestroyEnvironmentBlock(env_block as _);
    }
    if let Some(err) = err {
        bail!("CreateProcessAsUserW failed: {err}");
    }
    CloseHandle(pi.hThread);
    if !job.is_null() {
        let _ = AssignProcessToJobObject(job, pi.hProcess);
    }
    Ok(ChildProcess {
        process: pi.hProcess,
    })
}

fn spawn_session_group(session_id: u32, config: &Config) -> Result<SessionGroup> {
    let exe = std::env::current_exe()?
        .to_string_lossy()
        .into_owned();
    let helper_port = pick_loopback_port()?;
    let helper_token = uuid::Uuid::new_v4().simple().to_string();

    let helper_cmd = format!(
        "{} --secure-helper --helper-port {} --helper-token {} --log-level {}",
        quote_arg(&exe),
        helper_port,
        quote_arg(&helper_token),
        quote_arg(&config.log_level),
    );
    let worker_cmd = format!(
        "{} --helper-port {} --helper-token {}",
        build_command_line(&exe, "--session-worker", config),
        helper_port,
        quote_arg(&helper_token),
    );

    let job = create_kill_on_close_job();

    // 1. Spawn LocalSystem + TokenUIAccess=1 secure-helper in the active interactive session.
    let helper = unsafe {
        let sys_tok = match create_system_session_token(session_id, true) {
            Ok(t) => t,
            Err(e) => {
                if !job.is_null() {
                    CloseHandle(job);
                }
                return Err(e);
            }
        };
        let res = spawn_with_token(sys_tok, &helper_cmd, false, job);
        CloseHandle(sys_tok);
        match res {
            Ok(h) => h,
            Err(e) => {
                if !job.is_null() {
                    CloseHandle(job);
                }
                return Err(e.context("spawn secure-helper"));
            }
        }
    };

    // Give secure-helper a brief moment to bind its loopback port.
    std::thread::sleep(Duration::from_millis(120));

    // 2. Spawn interactive session-worker under the logged-in user's token so Windows Graphics
    // Capture (WGC), Media Foundation H.264, WASAPI, and App Launcher work natively.
    // On a pre-login Lock Screen (no user token yet), fall back to LocalSystem until login.
    let (worker, user_mode) = unsafe {
        if let Some(user_tok) = create_user_session_token(session_id) {
            let res = spawn_with_token(user_tok, &worker_cmd, true, job);
            CloseHandle(user_tok);
            match res {
                Ok(w) => (w, true),
                Err(e) => {
                    drop(helper);
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    return Err(e.context("spawn interactive user session-worker"));
                }
            }
        } else {
            let sys_tok = match create_system_session_token(session_id, true) {
                Ok(t) => t,
                Err(e) => {
                    drop(helper);
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    return Err(e);
                }
            };
            let res = spawn_with_token(sys_tok, &worker_cmd, false, job);
            CloseHandle(sys_tok);
            match res {
                Ok(w) => (w, false),
                Err(e) => {
                    drop(helper);
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    return Err(e.context("spawn lock-screen fallback session-worker"));
                }
            }
        }
    };

    Ok(SessionGroup {
        session_id,
        user_mode,
        worker,
        helper,
        job,
    })
}

fn supervisor_loop(config: &Config) {
    let mut group: Option<SessionGroup> = None;
    let mut seen_bump = SESSION_BUMP.load(Ordering::Acquire);
    while !STOP_REQUESTED.load(Ordering::Acquire) {
        let bump = SESSION_BUMP.load(Ordering::Acquire);
        if bump != seen_bump {
            seen_bump = bump;
            tracing::info!("received Windows session-change notification");
        }
        let active_session = unsafe { WTSGetActiveConsoleSessionId() };
        if active_session == u32::MAX {
            group = None;
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let need_upgrade_to_user = group
            .as_ref()
            .is_some_and(|g| !g.user_mode && has_logged_in_user(active_session));
        let need_spawn = match &group {
            Some(g) if g.session_id == active_session && g.is_alive() && !need_upgrade_to_user => {
                false
            }
            _ => true,
        };

        if need_spawn {
            // Drop old worker + helper before starting a new pair so port 9876 is released cleanly.
            group = None;
            std::thread::sleep(Duration::from_millis(250));
            match spawn_session_group(active_session, config) {
                Ok(new_group) => {
                    tracing::info!(
                        session_id = active_session,
                        user_mode = new_group.user_mode,
                        "spawned interactive session worker + secure-helper"
                    );
                    group = Some(new_group);
                }
                Err(e) => {
                    tracing::error!(
                        session_id = active_session,
                        error = %e,
                        "failed to spawn session worker group"
                    );
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(500));
    }
    drop(group);
}
