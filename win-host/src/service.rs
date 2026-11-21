use crate::{
    config::{CaptureMode, Codec, Config},
    net::tls,
    security::{self, WorkerSecrets},
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::c_void,
    fs::File,
    io::{self, Write},
    mem,
    os::windows::io::{FromRawHandle, RawHandle},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        OnceLock,
    },
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, SetHandleInformation, ERROR_SERVICE_ALREADY_RUNNING,
        ERROR_SERVICE_EXISTS, HANDLE, HANDLE_FLAG_INHERIT, NO_ERROR, WAIT_OBJECT_0,
    },
    Security::{
        Authorization::ConvertSidToStringSidW, DuplicateTokenEx, GetTokenInformation,
        SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId, TokenUIAccess,
        TokenUser, SECURITY_ATTRIBUTES, TOKEN_ALL_ACCESS, TOKEN_USER,
    },
    System::{
        Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Pipes::CreatePipe,
        RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken},
        Services::*,
        Threading::{
            CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, ResumeThread,
            TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
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
        Codec::Hevc => "hevc",
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

/// Non-secret CLI arguments forwarded to `--session-worker`.
/// Auth token and TLS private key are NEVER placed on the command line; they travel over an
/// inherited anonymous stdin pipe.
fn non_secret_worker_args(config: &Config) -> Vec<String> {
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
        "--secrets-from-stdin".into(),
    ];
    if config.capture_cursor {
        args.push("--capture-cursor".into());
    }
    if config.no_tls {
        args.push("--no-tls".into());
    }
    if config.allow_insecure_no_auth {
        args.push("--allow-insecure-no-auth".into());
    }
    if config.no_discovery {
        args.push("--no-discovery".into());
    }
    for cidr in &config.allow_cidr {
        args.push("--allow-cidr".into());
        args.push(cidr.clone());
    }
    args
}

fn build_worker_command_line(exe: &str, config: &Config, helper_pipe: &str) -> String {
    let mut parts = vec![quote_arg(exe), "--session-worker".to_owned()];
    for arg in non_secret_worker_args(config) {
        parts.push(quote_arg(&arg));
    }
    parts.push("--helper-pipe".into());
    parts.push(quote_arg(helper_pipe));
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
/// Stores token & config in an ACL-protected `C:\ProgramData\WinBridge\service.conf` file
/// (SYSTEM + Administrators only) rather than SCM `ImagePath`.
pub fn install(config: &Config) -> Result<()> {
    let mut effective = Config {
        bind: config.bind,
        port: config.port,
        token: config.token.clone(),
        token_file: config.token_file.clone(),
        allow_insecure_no_auth: config.allow_insecure_no_auth,
        no_tls: config.no_tls,
        tls_dir: config.tls_dir.clone(),
        allow_cidr: config.allow_cidr.clone(),
        no_discovery: config.no_discovery,
        fps: config.fps,
        bitrate: config.bitrate,
        codec: config.codec,
        capture_mode: config.capture_mode,
        capture_cursor: config.capture_cursor,
        log_level: config.log_level.clone(),
        list_windows: false,
        test_capture: None,
        test_input: None,
        delay_secs: 0,
        frames: 30,
        save: None,
        install_service: false,
        uninstall_service: false,
        service: false,
        session_worker: false,
        secrets_from_stdin: false,
        secure_helper: false,
        helper_pipe: None,
        allowed_worker_pid: None,
        allowed_user_sid: None,
    };

    let mut generated_token = false;
    if effective.token.is_empty() && !effective.allow_insecure_no_auth {
        // Preserve existing token when updating service without re-passing --token.
        let mut existing = Config {
            bind: effective.bind,
            port: effective.port,
            token: String::new(),
            token_file: None,
            allow_insecure_no_auth: false,
            no_tls: false,
            tls_dir: None,
            allow_cidr: Vec::new(),
            no_discovery: effective.no_discovery,
            fps: effective.fps,
            bitrate: effective.bitrate,
            codec: effective.codec,
            capture_mode: effective.capture_mode,
            capture_cursor: effective.capture_cursor,
            log_level: effective.log_level.clone(),
            list_windows: false,
            test_capture: None,
            test_input: None,
            delay_secs: 0,
            frames: 30,
            save: None,
            install_service: false,
            uninstall_service: false,
            service: false,
            session_worker: false,
            secrets_from_stdin: false,
            secure_helper: false,
            helper_pipe: None,
            allowed_worker_pid: None,
            allowed_user_sid: None,
        };
        if security::load_service_config(&mut existing).unwrap_or(false)
            && !existing.token.is_empty()
        {
            effective.token = existing.token;
        } else {
            effective.token = security::generate_token()?;
            generated_token = true;
        }
    }
    security::validate_auth_policy(&effective)?;

    let conf_path = security::save_service_config(&effective)?;
    let tls_dir = effective
        .tls_dir
        .clone()
        .unwrap_or_else(|| security::program_data_dir().join("tls"));
    let tls_mat = if !effective.no_tls {
        Some(tls::load_or_create(&tls_dir, true)?)
    } else {
        None
    };

    let exe = std::env::current_exe()
        .context("locate current win-host.exe")?
        .to_string_lossy()
        .into_owned();
    // Keep SCM ImagePath completely free of secrets.
    let bin_path = format!("{} --service", quote_arg(&exe));
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

    println!(
        "Service '{SERVICE_NAME}' is running on {}:{} (UAC / Secure Desktop enabled).",
        effective.bind, effective.port
    );
    println!("Config file (SYSTEM/Admins ACL): {}", conf_path.display());
    if generated_token {
        println!("Generated Auth Token (copy to Mac client): {}", effective.token);
    }
    if let Some(mat) = tls_mat {
        println!("TLS 1.3 Enabled — SHA-256 Fingerprint: {}", mat.fingerprint);
    } else {
        println!("TLS Disabled (--no-tls).");
    }
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
pub fn run_service_dispatcher(mut config: Config) -> Result<()> {
    // Load ACL-protected C:\ProgramData\WinBridge\service.conf if present; CLI args still act as
    // legacy fallback if migrating from an older service registration.
    if let Err(e) = security::load_service_config(&mut config) {
        tracing::error!(error = %e, "failed to load service.conf");
    }
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
        _ => 120,
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
    pid: u32,
    suspended_thread: Option<HANDLE>,
}

impl ChildProcess {
    fn is_alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 }
    }

    fn resume(&mut self) {
        if let Some(thread) = self.suspended_thread.take() {
            unsafe {
                ResumeThread(thread);
                CloseHandle(thread);
            }
        }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        unsafe {
            if let Some(thread) = self.suspended_thread.take() {
                CloseHandle(thread);
            }
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

unsafe fn token_user_sid_string(token: HANDLE) -> Option<String> {
    let mut len = 0u32;
    GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len);
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
        return None;
    }
    let user = &*(buf.as_ptr() as *const TOKEN_USER);
    let mut sid_ptr: *mut u16 = ptr::null_mut();
    if ConvertSidToStringSidW(user.User.Sid, &mut sid_ptr) == 0 || sid_ptr.is_null() {
        return None;
    }
    let mut n = 0usize;
    while *sid_ptr.add(n) != 0 {
        n += 1;
    }
    let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_ptr, n));
    LocalFree(sid_ptr.cast());
    Some(sid)
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
    stdin_payload: Option<&[u8]>,
    suspended: bool,
) -> Result<ChildProcess> {
    let mut cmd = to_wide(cmd_line);
    let mut desktop = to_wide("winsta0\\default");

    let mut env_block: *mut c_void = ptr::null_mut();
    let has_env = use_user_env
        && CreateEnvironmentBlock(&mut env_block, token, 0) != 0
        && !env_block.is_null();
    let mut flags = CREATE_NO_WINDOW | if has_env { CREATE_UNICODE_ENVIRONMENT } else { 0 };
    if suspended {
        flags |= CREATE_SUSPENDED;
    }

    let mut read_pipe: HANDLE = ptr::null_mut();
    let mut write_pipe: HANDLE = ptr::null_mut();
    let mut inherit_handles = 0;

    if let Some(payload) = stdin_payload {
        let sa = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 1,
        };
        let buf_size = (payload.len() + 64).max(4096) as u32;
        if CreatePipe(&mut read_pipe, &mut write_pipe, &sa, buf_size) == 0 {
            if has_env {
                DestroyEnvironmentBlock(env_block as _);
            }
            bail!("CreatePipe for worker secrets failed: {}", io::Error::last_os_error());
        }
        SetHandleInformation(write_pipe, HANDLE_FLAG_INHERIT, 0);
        // Write the small secrets frame into the kernel pipe buffer and close the write end so
        // the child reads cleanly to EOF without blocking the parent.
        {
            let mut writer = File::from_raw_handle(write_pipe as RawHandle);
            write_pipe = ptr::null_mut();
            if let Err(e) = writer.write_all(payload) {
                CloseHandle(read_pipe);
                if has_env {
                    DestroyEnvironmentBlock(env_block as _);
                }
                return Err(e).context("write worker secrets pipe");
            }
        }
        inherit_handles = 1;
    }

    let mut si: STARTUPINFOW = mem::zeroed();
    si.cb = mem::size_of::<STARTUPINFOW>() as u32;
    si.lpDesktop = desktop.as_mut_ptr();
    if !read_pipe.is_null() {
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = read_pipe;
        si.hStdOutput = ptr::null_mut();
        si.hStdError = ptr::null_mut();
    }
    let mut pi: PROCESS_INFORMATION = mem::zeroed();

    let created = CreateProcessAsUserW(
        token,
        ptr::null(),
        cmd.as_mut_ptr(),
        ptr::null(),
        ptr::null(),
        inherit_handles,
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
    if !read_pipe.is_null() {
        CloseHandle(read_pipe);
    }
    if !write_pipe.is_null() {
        CloseHandle(write_pipe);
    }
    if has_env {
        DestroyEnvironmentBlock(env_block as _);
    }
    if let Some(err) = err {
        bail!("CreateProcessAsUserW failed: {err}");
    }
    if !job.is_null() {
        let _ = AssignProcessToJobObject(job, pi.hProcess);
    }
    let suspended_thread = if suspended {
        Some(pi.hThread)
    } else {
        CloseHandle(pi.hThread);
        None
    };
    Ok(ChildProcess {
        process: pi.hProcess,
        pid: pi.dwProcessId,
        suspended_thread,
    })
}

fn spawn_session_group(session_id: u32, config: &Config) -> Result<SessionGroup> {
    let exe = std::env::current_exe()?
        .to_string_lossy()
        .into_owned();
    let helper_pipe = format!(r"\\.\pipe\WinBridgeHelper-{}", uuid::Uuid::new_v4().simple());

    // Load (or generate) persistent SYSTEM+Admin TLS identity in Session 0 so both Lock Screen
    // and interactive user workers present the exact same certificate fingerprint without ever
    // exposing key.pem on disk to unprivileged users.
    let (tls_cert_pem, tls_key_pem) = if !config.no_tls {
        let tls_dir = config
            .tls_dir
            .clone()
            .unwrap_or_else(|| security::program_data_dir().join("tls"));
        let mat = tls::load_or_create(&tls_dir, true)?;
        (mat.cert_pem, mat.key_pem)
    } else {
        (String::new(), String::new())
    };
    let secret_bytes = WorkerSecrets {
        auth_token: config.token.clone(),
        tls_cert_pem,
        tls_key_pem,
    }
    .encode_bytes();

    let worker_cmd = build_worker_command_line(&exe, config, &helper_pipe);
    let job = create_kill_on_close_job();

    // 1. Spawn --session-worker SUSPENDED first so we know its exact PID before launching
    // --secure-helper. Secrets travel exclusively via inherited anonymous stdin pipe.
    let (mut worker, user_mode, user_sid) = unsafe {
        if let Some(user_tok) = create_user_session_token(session_id) {
            let sid = token_user_sid_string(user_tok);
            let res = spawn_with_token(
                user_tok,
                &worker_cmd,
                true,
                job,
                Some(&secret_bytes),
                true,
            );
            CloseHandle(user_tok);
            match res {
                Ok(w) => (w, true, sid),
                Err(e) => {
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
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    return Err(e);
                }
            };
            let res = spawn_with_token(
                sys_tok,
                &worker_cmd,
                false,
                job,
                Some(&secret_bytes),
                true,
            );
            CloseHandle(sys_tok);
            match res {
                Ok(w) => (w, false, None),
                Err(e) => {
                    if !job.is_null() {
                        CloseHandle(job);
                    }
                    return Err(e.context("spawn lock-screen fallback session-worker"));
                }
            }
        }
    };

    // 2. Spawn LocalSystem + TokenUIAccess=1 --secure-helper locked to worker's exact PID and SID.
    let mut helper_cmd = format!(
        "{} --secure-helper --helper-pipe {} --allowed-worker-pid {} --log-level {}",
        quote_arg(&exe),
        quote_arg(&helper_pipe),
        worker.pid,
        quote_arg(&config.log_level),
    );
    if let Some(ref sid) = user_sid {
        helper_cmd.push_str(" --allowed-user-sid ");
        helper_cmd.push_str(&quote_arg(sid));
    }

    let helper = unsafe {
        let sys_tok = match create_system_session_token(session_id, true) {
            Ok(t) => t,
            Err(e) => {
                drop(worker);
                if !job.is_null() {
                    CloseHandle(job);
                }
                return Err(e);
            }
        };
        let res = spawn_with_token(sys_tok, &helper_cmd, false, job, None, false);
        CloseHandle(sys_tok);
        match res {
            Ok(h) => h,
            Err(e) => {
                drop(worker);
                if !job.is_null() {
                    CloseHandle(job);
                }
                return Err(e.context("spawn secure-helper"));
            }
        }
    };

    // Give secure-helper a brief moment to create the Named Pipe before resuming session-worker.
    std::thread::sleep(Duration::from_millis(80));
    worker.resume();

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
            group = None;
            std::thread::sleep(Duration::from_millis(250));
            match spawn_session_group(active_session, config) {
                Ok(new_group) => {
                    tracing::info!(
                        session_id = active_session,
                        user_mode = new_group.user_mode,
                        worker_pid = new_group.worker.pid,
                        helper_pid = new_group.helper.pid,
                        "spawned interactive session worker + PID-verified Named Pipe secure-helper"
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
