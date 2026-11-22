use super::{
    cursor::Tracker,
    scoped_clipboard::{Command, Scope, Transactions},
};
use crate::proto::{ClipboardRequest, ClipboardSync, CursorShape};
use anyhow::{anyhow, ensure, Context, Result};
use std::{
    cell::RefCell,
    io, mem, ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as sync, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::Sender;
use windows_sys::Win32::{
    Foundation::HWND,
    System::{Com::*, Threading::GetCurrentThreadId},
    UI::{Accessibility::*, WindowsAndMessaging::*},
};

pub enum Event {
    Changed(u64),
    Hidden(u64),
    Cursor(CursorShape),
    Error(String),
}

thread_local! {static EVENTS: RefCell<Option<Sender<Event>>>=const {RefCell::new(None)};}
unsafe extern "system" fn win_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if hwnd.is_null() || object != OBJID_WINDOW || child != 0 {
        return;
    }
    let id = hwnd as usize as u64;
    let notification = match event {
        EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY => Event::Hidden(id),
        EVENT_OBJECT_CREATE
        | EVENT_OBJECT_SHOW
        | EVENT_OBJECT_LOCATIONCHANGE
        | EVENT_OBJECT_NAMECHANGE
        | EVENT_SYSTEM_FOREGROUND => Event::Changed(id),
        _ => return,
    };
    EVENTS.with(|slot| {
        if let Some(events) = slot.borrow().as_ref() {
            let _ = events.try_send(notification);
        }
    });
}

struct Resources {
    hwnd: HWND,
    hooks: Vec<HWINEVENTHOOK>,
}
impl Resources {
    fn new() -> Result<Self> {
        unsafe {
            let hr = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32);
            ensure!(hr >= 0, "CoInitializeEx failed: 0x{hr:x}");
            let mut r = Self {
                hwnd: ptr::null_mut(),
                hooks: vec![],
            };
            r.hwnd = CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            ensure!(
                !r.hwnd.is_null(),
                "CreateWindowExW: {}",
                io::Error::last_os_error()
            );
            ensure!(SetTimer(r.hwnd, 1, 20, None) != 0, "SetTimer failed");
            for (first, last) in [
                (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
                (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
                (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            ] {
                let h = SetWinEventHook(
                    first,
                    last,
                    ptr::null_mut(),
                    Some(win_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                ensure!(!h.is_null(), "SetWinEventHook failed");
                r.hooks.push(h);
            }
            Ok(r)
        }
    }
}
impl Drop for Resources {
    fn drop(&mut self) {
        unsafe {
            for h in self.hooks.drain(..) {
                UnhookWinEvent(h);
            }
            if !self.hwnd.is_null() {
                KillTimer(self.hwnd, 1);
                DestroyWindow(self.hwnd);
            }
            CoUninitialize();
        }
    }
}

pub struct Observer {
    id: u32,
    commands: sync::SyncSender<Command>,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Observer {
    pub fn new(events: Sender<Event>) -> Result<Self> {
        let (tx, rx) = sync::sync_channel(4);
        let (started, ready) = sync::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = stopped.clone();
        let thread = thread::Builder::new()
            .name("desktop-observer".into())
            .spawn(move || {
                let resources = match Resources::new() {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = started.send(Err(e));
                        return;
                    }
                };
                EVENTS.with(|slot| *slot.borrow_mut() = Some(events.clone()));
                let _ = started.send(Ok(unsafe { GetCurrentThreadId() }));
                observer_loop(&resources, events, rx, &worker_stop);
                EVENTS.with(|slot| *slot.borrow_mut() = None);
            })?;
        let id = ready
            .recv()
            .context("observer initialization interrupted")??;
        Ok(Self {
            id,
            commands: tx,
            stopped,
            thread: Some(thread),
        })
    }
    pub(crate) async fn clipboard(
        &self,
        request: ClipboardRequest,
        scope: Scope,
    ) -> Result<Option<ClipboardSync>> {
        let (reply, wait) = tokio::sync::oneshot::channel();
        self.commands
            .try_send(Command {
                request,
                scope,
                reply,
                deadline: Instant::now() + Duration::from_secs(2),
            })
            .map_err(|_| anyhow!("clipboard worker busy"))?;
        unsafe {
            PostThreadMessageW(self.id, WM_APP, 0, 0);
        }
        tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .context("clipboard timed out")??
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        unsafe {
            PostThreadMessageW(self.id, WM_QUIT, 0, 0);
        }
        if let Some(t) = self.thread.take() {
            if tokio::runtime::Handle::try_current()
                .is_ok_and(|rt| rt.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
            {
                let _ = tokio::task::block_in_place(|| t.join());
            } else {
                let _ = t.join();
            }
        }
    }
}

fn observer_loop(
    resources: &Resources,
    events: Sender<Event>,
    commands: sync::Receiver<Command>,
    stopped: &AtomicBool,
) {
    let mut cursor = Tracker::new();
    let mut transactions = Transactions::default();
    let mut msg: MSG = unsafe { mem::zeroed() };
    while !stopped.load(Ordering::Acquire)
        && unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) } > 0
    {
        if stopped.load(Ordering::Acquire) {
            break;
        }
        match msg.message {
            WM_TIMER | WM_APP => {
                for command in commands.try_iter() {
                    if command.reply.is_closed() || Instant::now() > command.deadline {
                        continue;
                    }
                    let result =
                        transactions.execute(resources.hwnd, command.request, command.scope);
                    let _ = command.reply.send(result);
                }
                cursor.sample(&events);
            }
            _ => unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            },
        }
    }
}
