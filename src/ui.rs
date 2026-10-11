use haveigotit::{
    db::Folder,
    engine::{self, Command, Input, Output, Status},
    matching::{Alert, Reason},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::c_void,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{mpsc::SyncSender, Arc, Mutex},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::*, Threading::*},
    UI::{HiDpi::*, Shell::*, WindowsAndMessaging::*},
};

const WAKE: u32 = WM_APP + 1;
const TRAY: u32 = WM_APP + 2;
const ADD: u16 = 101;
const REMOVE: u16 = 102;
const RESCAN: u16 = 103;
const LANGUAGE: u16 = 104;
const OPEN: u16 = 105;
const IGNORE: u16 = 106;
const SHOW: u16 = 201;
const EXIT: u16 = 202;
thread_local! {static APP:RefCell<Option<App>>=const {RefCell::new(None)};}

struct App {
    main: HWND,
    popup: HWND,
    main_controls: Vec<HWND>,
    popup_controls: Vec<HWND>,
    fonts: Vec<HFONT>,
    tx: SyncSender<Input>,
    inbox: Arc<Mutex<VecDeque<Output>>>,
    folders: Vec<Folder>,
    count: u64,
    zh: bool,
    alerts: VecDeque<Alert>,
    current: Option<Alert>,
    last_error: Option<String>,
    scan_message: Option<String>,
    taskbar: u32,
}
enum Action {
    None,
    Add,
    Reveal(PathBuf),
    Menu,
    Exit,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn shell_wide(path: &Path) -> Option<Vec<u16>> {
    let original: Vec<u16> = path.as_os_str().encode_wide().collect();
    let mut units = haveigotit::shell_paths::display_path(&original)?;
    units.push(0);
    Some(units)
}
fn display(path: &Path) -> String {
    let original: Vec<u16> = path.as_os_str().encode_wide().collect();
    let units = haveigotit::shell_paths::display_path(&original).unwrap_or(original);
    String::from_utf16_lossy(&units)
}

fn text(zh: bool, en: &str, cn: &str) -> String {
    if zh {
        cn.into()
    } else {
        en.into()
    }
}
unsafe fn app_icon() -> HICON {
    // Win32 MAKEINTRESOURCEW(1): this value is an integer resource identifier,
    // not a dereferenceable pointer.
    let icon = LoadIconW(GetModuleHandleW(null()), std::ptr::without_provenance(1));
    if icon.is_null() {
        LoadIconW(null_mut(), IDI_INFORMATION)
    } else {
        icon
    }
}
unsafe fn set_text(hwnd: HWND, value: &str) {
    SetWindowTextW(hwnd, wide(value).as_ptr());
}
unsafe fn move_control(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    MoveWindow(hwnd, x, y, w.max(1), h.max(1), 1);
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let mutex = CreateMutexW(null(), 0, wide("Local\\HaveIGotIt-v0.1").as_ptr());
        if mutex.is_null() {
            return;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let existing = FindWindowW(wide("HaveIGotIt.Settings").as_ptr(), null());
            if !existing.is_null() {
                ShowWindow(existing, SW_SHOW);
                SetForegroundWindow(existing);
            }
            CloseHandle(mutex);
            CoUninitialize();
            return;
        }
        let Some(local) = std::env::var_os("LOCALAPPDATA") else {
            MessageBoxW(
                null_mut(),
                wide("LOCALAPPDATA is unavailable").as_ptr(),
                wide("HaveIGotIt").as_ptr(),
                MB_ICONERROR,
            );
            CloseHandle(mutex);
            CoUninitialize();
            return;
        };
        let directory = PathBuf::from(local).join("HaveIGotIt");
        if let Err(e) = std::fs::create_dir_all(&directory) {
            MessageBoxW(
                null_mut(),
                wide(&e.to_string()).as_ptr(),
                wide("HaveIGotIt").as_ptr(),
                MB_ICONERROR,
            );
            CloseHandle(mutex);
            CoUninitialize();
            return;
        }
        let instance = GetModuleHandleW(null());
        for class in ["HaveIGotIt.Settings", "HaveIGotIt.Alarm"] {
            let name = wide(class);
            let mut wc: WNDCLASSEXW = zeroed();
            wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(window_proc);
            wc.hInstance = instance;
            wc.lpszClassName = name.as_ptr();
            wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
            wc.hIcon = app_icon();
            wc.hbrBackground = (COLOR_WINDOW + 1) as HBRUSH;
            if RegisterClassExW(&wc) == 0 {
                MessageBoxW(
                    null_mut(),
                    wide("Cannot register window class").as_ptr(),
                    wide("HaveIGotIt").as_ptr(),
                    MB_ICONERROR,
                );
                CloseHandle(mutex);
                CoUninitialize();
                return;
            }
        }
        let main = CreateWindowExW(
            0,
            wide("HaveIGotIt.Settings").as_ptr(),
            wide("HaveIGotIt?").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            760,
            500,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        let popup = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            wide("HaveIGotIt.Alarm").as_ptr(),
            wide("HaveIGotIt?").as_ptr(),
            WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
            0,
            0,
            480,
            300,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if main.is_null() || popup.is_null() {
            CloseHandle(mutex);
            CoUninitialize();
            return;
        }
        let inbox = Arc::new(Mutex::new(VecDeque::new()));
        let queue = inbox.clone();
        let receiver = main as usize;
        let tx = engine::start(directory.join("index.sqlite3"), move |output| {
            if let Ok(mut queue) = queue.lock() {
                // Status snapshots replace older snapshots; alarms are retained
                // up to a bounded limit to avoid unbounded storm memory use.
                if matches!(&output, Output::Status(_)) {
                    queue.retain(|x| !matches!(x, Output::Status(_)));
                }
                if queue.len() >= 256 {
                    queue.retain(|x| !matches!(x, Output::Error(_)));
                    if queue.len() >= 256 {
                        queue.pop_front();
                    }
                }
                queue.push_back(output);
            }
            PostMessageW(receiver as HWND, WAKE, 0, 0);
        });
        let mut main_controls = Vec::new();
        main_controls.push(control(main, "STATIC", "HaveIGotIt?", 0, 0));
        main_controls.push(control(main, "STATIC", "", 0, 0));
        main_controls.push(control(
            main,
            "LISTBOX",
            "",
            LBS_NOTIFY as u32 | WS_VSCROLL | WS_BORDER,
            110,
        ));
        for (id, label) in [
            (ADD, "Add folder"),
            (REMOVE, "Remove"),
            (RESCAN, "Rebuild index"),
            (LANGUAGE, "简体中文"),
        ] {
            main_controls.push(control(main, "BUTTON", label, BS_PUSHBUTTON as u32, id));
        }
        main_controls.push(control(main, "STATIC", "", 0, 0));
        let popup_controls = vec![
            control(popup, "STATIC", "", 0, 0),
            control(
                popup,
                "EDIT",
                "",
                ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL,
                0,
            ),
            control(popup, "BUTTON", "Open Location", BS_PUSHBUTTON as u32, OPEN),
            control(popup, "BUTTON", "Ignore", BS_PUSHBUTTON as u32, IGNORE),
        ];
        let taskbar = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        APP.with(|cell| {
            *cell.borrow_mut() = Some(App {
                main,
                popup,
                main_controls,
                popup_controls,
                fonts: Vec::new(),
                tx,
                inbox,
                folders: Vec::new(),
                count: 0,
                zh: cfg!(feature = "zh-release"),
                alerts: VecDeque::new(),
                current: None,
                last_error: None,
                scan_message: None,
                taskbar,
            })
        });
        APP.with(|cell| {
            let mut guard = cell.borrow_mut();
            let app = guard.as_mut().unwrap();
            app.tray(NIM_ADD);
            app.layout();
            app.translate();
        });
        let background = std::env::args().any(|arg| arg == "--background");
        if !background {
            ShowWindow(main, SW_SHOW);
        }
        // Drain early startup outputs even if they arrived before APP existed.
        PostMessageW(main, WAKE, 0, 0);
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(main, &msg) == 0 && IsDialogMessageW(popup, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        APP.with(|cell| {
            if let Some(app) = cell.borrow_mut().take() {
                let _ = app.tx.try_send(Input::Command(Command::Stop));
                app.tray(NIM_DELETE);
                DestroyWindow(app.popup);
                for font in app.fonts {
                    DeleteObject(font);
                }
            }
        });
        DestroyWindow(main);
        CloseHandle(mutex);
        CoUninitialize();
    }
}

unsafe fn control(parent: HWND, class: &str, label: &str, flags: u32, id: u16) -> HWND {
    CreateWindowExW(
        0,
        wide(class).as_ptr(),
        wide(label).as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | flags,
        0,
        0,
        1,
        1,
        parent,
        id as usize as HMENU,
        GetModuleHandleW(null()),
        null(),
    )
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CTLCOLORSTATIC {
        SetBkColor(wparam as HDC, 0x00ffffff);
        SetTextColor(wparam as HDC, 0x00282018);
        return GetStockObject(WHITE_BRUSH) as isize;
    }
    let mut handled = false;
    let action = APP.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return Action::None;
        };
        let Some(app) = guard.as_mut() else {
            return Action::None;
        };
        handled = true;
        if msg == app.taskbar {
            app.tray(NIM_ADD);
            return Action::None;
        }
        match msg {
            WM_CLOSE => {
                if hwnd == app.main {
                    ShowWindow(hwnd, SW_HIDE);
                } else {
                    app.dismiss();
                }
                Action::None
            }
            WM_SIZE => {
                if hwnd == app.main && wparam == SIZE_MINIMIZED as usize {
                    ShowWindow(hwnd, SW_HIDE);
                } else {
                    app.layout();
                }
                Action::None
            }
            WM_DPICHANGED => {
                let rect = &*(lparam as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                if hwnd == app.popup {
                    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut info: MONITORINFO = zeroed();
                    info.cbSize = size_of::<MONITORINFO>() as u32;
                    if GetMonitorInfoW(monitor, &mut info) != 0 {
                        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                        let w = (480 * dpi / 96).min(info.rcWork.right - info.rcWork.left - 20);
                        let h = (340 * dpi / 96).min(info.rcWork.bottom - info.rcWork.top - 20);
                        SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            info.rcWork.right - w - 10,
                            info.rcWork.bottom - h - 10,
                            w,
                            h,
                            SWP_NOACTIVATE,
                        );
                    }
                }
                app.layout();
                Action::None
            }
            WM_GETMINMAXINFO => {
                if hwnd == app.main {
                    let info = &mut *(lparam as *mut MINMAXINFO);
                    let dpi = GetDpiForWindow(hwnd) as i32;
                    info.ptMinTrackSize.x = 660 * dpi / 96;
                    info.ptMinTrackSize.y = 440 * dpi / 96;
                }
                Action::None
            }
            WM_COMMAND => match (wparam & 65535) as u16 {
                ADD => Action::Add,
                REMOVE => {
                    let selected = SendMessageW(app.main_controls[2], LB_GETCURSEL, 0, 0);
                    if selected >= 0 {
                        if let Some(folder) = app.folders.get(selected as usize) {
                            app.send(Command::Remove(folder.id));
                        }
                    }
                    Action::None
                }
                RESCAN => {
                    app.last_error = None;
                    app.send(Command::Rescan);
                    Action::None
                }
                LANGUAGE => {
                    app.zh = !app.zh;
                    app.translate();
                    app.send(Command::Language(if app.zh { "zh" } else { "en" }.into()));
                    Action::None
                }
                OPEN => {
                    let path = app
                        .current
                        .as_ref()
                        .map(|alert| alert.old_file.path.clone());
                    if let Some(path) = path {
                        Action::Reveal(path)
                    } else {
                        Action::None
                    }
                }
                IGNORE => {
                    app.dismiss();
                    Action::None
                }
                SHOW => {
                    ShowWindow(app.main, SW_SHOW);
                    SetForegroundWindow(app.main);
                    Action::None
                }
                EXIT => Action::Exit,
                _ => {
                    handled = false;
                    Action::None
                }
            },
            TRAY => {
                match lparam as u32 {
                    WM_LBUTTONDBLCLK => {
                        ShowWindow(app.main, SW_SHOW);
                        SetForegroundWindow(app.main);
                    }
                    WM_RBUTTONUP => return Action::Menu,
                    _ => {}
                }
                Action::None
            }
            WAKE => {
                app.receive();
                Action::None
            }
            WM_DESTROY if hwnd == app.main => {
                PostQuitMessage(0);
                Action::None
            }
            _ => {
                handled = false;
                Action::None
            }
        }
    });
    // Modal folder/error dialogs run after releasing the RefCell borrow. Their
    // nested message loops can therefore keep draining background events safely.
    match action {
        Action::Menu => {
            let (main, zh) = APP.with(|cell| {
                let guard = cell.borrow();
                let app = guard.as_ref().unwrap();
                (app.main, app.zh)
            });
            let menu = CreatePopupMenu();
            AppendMenuW(
                menu,
                MF_STRING,
                SHOW as usize,
                wide(&text(zh, "Settings", "设置")).as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_STRING,
                EXIT as usize,
                wide(&text(zh, "Exit", "退出")).as_ptr(),
            );
            let mut point: POINT = zeroed();
            GetCursorPos(&mut point);
            SetForegroundWindow(main);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                main,
                null(),
            );
            DestroyMenu(menu);
            PostMessageW(main, WM_NULL, 0, 0);
            if choice == EXIT as i32 {
                PostQuitMessage(0);
            }
            if choice == SHOW as i32 {
                ShowWindow(main, SW_SHOW);
                SetForegroundWindow(main);
            }
        }
        Action::Add => {
            if let Some(path) = choose_folder(hwnd) {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        app.send(Command::Add(path));
                    }
                });
            }
        }
        Action::Reveal(path) => {
            let valid = APP.with(|cell| {
                cell.borrow().as_ref().is_some_and(|app| {
                    app.folders
                        .iter()
                        .any(|f| haveigotit::files::permitted(&path, &f.path))
                })
            });
            if valid && path.is_file() {
                match reveal(&path) {
                    Ok(selected) => APP.with(|cell| {
                        if let Some(app) = cell.borrow_mut().as_mut() {
                            if !selected {
                                app.last_error = Some(text(
                                    app.zh,
                                    "Opened the containing folder; file selection was unavailable",
                                    "已打开所在文件夹；未能自动选中文件",
                                ));
                            }
                            app.dismiss();
                            app.translate();
                        }
                    }),
                    Err(error) => {
                        MessageBoxW(
                            hwnd,
                            wide(&format!(
                                "Could not open Explorer / 无法打开文件位置\r\n{error}"
                            ))
                            .as_ptr(),
                            wide("HaveIGotIt").as_ptr(),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
            } else {
                APP.with(|cell| {
                    if let Some(app) = cell.borrow_mut().as_mut() {
                        app.dismiss();
                        app.translate();
                    }
                });
            }
        }
        Action::Exit => {
            PostQuitMessage(0);
        }
        Action::None => {}
    }
    if handled {
        0
    } else {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

impl App {
    unsafe fn send(&mut self, command: Command) {
        if self.tx.try_send(Input::Command(command)).is_err() {
            self.last_error = Some(text(
                self.zh,
                "Busy; please try again shortly",
                "程序繁忙，请稍后重试",
            ));
            self.translate();
        }
    }
    unsafe fn tray(&self, operation: u32) {
        let mut data: NOTIFYICONDATAW = zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.main;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = TRAY;
        data.hIcon = app_icon();
        let tooltip = wide("HaveIGotIt? — local download memory");
        for (target, source) in data.szTip.iter_mut().zip(tooltip) {
            *target = source;
        }
        Shell_NotifyIconW(operation, &data);
    }
    unsafe fn receive(&mut self) {
        let outputs: Vec<_> = self
            .inbox
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default();
        for output in outputs {
            match output {
                Output::Status(Status {
                    folders,
                    files,
                    language,
                    message,
                }) => {
                    let previous_index = SendMessageW(self.main_controls[2], LB_GETCURSEL, 0, 0);
                    let previous_id = if previous_index >= 0 {
                        self.folders.get(previous_index as usize).map(|f| f.id)
                    } else {
                        None
                    };
                    self.folders = folders;
                    self.count = files;
                    self.scan_message = message;
                    if let Some(language) = language {
                        self.zh = language == "zh";
                    }
                    SendMessageW(self.main_controls[2], LB_RESETCONTENT, 0, 0);
                    for folder in &self.folders {
                        SendMessageW(
                            self.main_controls[2],
                            LB_ADDSTRING,
                            0,
                            wide(&display(&folder.path)).as_ptr() as isize,
                        );
                    }
                    if let Some(index) =
                        previous_id.and_then(|id| self.folders.iter().position(|f| f.id == id))
                    {
                        SendMessageW(self.main_controls[2], LB_SETCURSEL, index, 0);
                    }
                    self.prune_alerts();
                }
                Output::Error(message) => self.last_error = Some(message),
                Output::Stopped => {}
                Output::DownloadMoved { id, path } => {
                    for alert in self.current.iter_mut().chain(self.alerts.iter_mut()) {
                        if alert.download_id == Some(id) {
                            alert.new_file.path = path.clone();
                        }
                    }
                }
                Output::DownloadFinished { id, alert } => {
                    if haveigotit::downloads::finish_alerts(
                        &mut self.current,
                        &mut self.alerts,
                        id,
                        alert.as_deref(),
                    ) {
                        self.next_alert();
                    }
                }
                Output::Alert(alert) => {
                    if self.alerts.len() < 128 {
                        self.alerts.push_back(*alert);
                    } else {
                        self.last_error = Some(text(
                            self.zh,
                            "Notification queue full; some alerts were skipped",
                            "提醒队列已满，部分提醒被跳过",
                        ));
                    }
                }
            }
        }
        if self.current.is_none() {
            self.next_alert();
        }
        self.translate();
    }
    unsafe fn next_alert(&mut self) {
        while let Some(alert) = self.alerts.pop_front() {
            if haveigotit::downloads::alert_valid(&alert, &self.folders)
                && alert.old_file.path.is_file()
            {
                self.current = Some(alert);
                self.place_popup();
                self.layout();
                self.translate();
                ShowWindow(self.popup, SW_SHOWNOACTIVATE);
                return;
            }
        }
        ShowWindow(self.popup, SW_HIDE);
    }
    unsafe fn prune_alerts(&mut self) {
        if haveigotit::downloads::prune_alerts(&mut self.current, &mut self.alerts, &self.folders) {
            self.next_alert();
        }
    }
    unsafe fn dismiss(&mut self) {
        self.current = None;
        self.next_alert();
    }
    unsafe fn translate(&mut self) {
        set_text(
            self.main_controls[1],
            &text(self.zh, "Your local download memory.", "本地下载记忆。"),
        );
        for (index, en, zh) in [
            (3, "Add folder", "添加文件夹"),
            (4, "Remove", "移除目录"),
            (5, "Rebuild index", "重建索引"),
            (6, "简体中文", "English"),
        ] {
            set_text(self.main_controls[index], &text(self.zh, en, zh));
        }
        let base = if self.zh {
            format!(
                "{} 个索引文件 · {} 个授权目录",
                self.count,
                self.folders.len()
            )
        } else {
            format!(
                "{} indexed files · {} authorized folders",
                self.count,
                self.folders.len()
            )
        };
        let detail = self.scan_message.as_ref().or(self.last_error.as_ref());
        set_text(
            self.main_controls[7],
            &detail.map_or(base.clone(), |detail| format!("{base}\r\n{detail}")),
        );
        if let Some(alert) = &self.current {
            let heading = if alert.reason == Reason::OlderVersion {
                text(
                    self.zh,
                    "An older package is still here",
                    "旧版本安装包还在这里",
                )
            } else {
                text(
                    self.zh,
                    "You may already have this",
                    "你可能已经有这个文件了",
                )
            };
            set_text(self.popup_controls[0], &heading);
            let download_target = haveigotit::downloads::target(&alert.new_file.path);
            let filename = download_target
                .as_ref()
                .unwrap_or(&alert.new_file.path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let reason = match alert.reason {
                Reason::Downloading => text(
                    self.zh,
                    "Download started. An existing file has the same name. Final size, version and content are not yet verified.",
                    "下载已开始，已有同名文件。最终大小、版本及内容尚未核对。",
                ),
                Reason::SampleMatch => text(
                    self.zh,
                    "Same normalized name, size and sampled content. Possible duplicate.",
                    "规范化名称、大小及抽样内容一致，可能重复。",
                ),
                Reason::SameVersion => text(
                    self.zh,
                    "Same name and known version; size differs by no more than 10%.",
                    "名称和已知版本一致，大小差异不超过 10%。",
                ),
                Reason::OlderVersion => text(
                    self.zh,
                    "Same package, different version. This is an age advisory.",
                    "同一安装包的不同版本；这是年龄提醒。",
                ),
            };
            let version = alert
                .old_file
                .name
                .version
                .as_ref()
                .or(alert.old_file.pe_version.as_ref());
            let mut body = format!(
                "{filename}\r\n\r\n{}\r\n{}\r\n{} MiB{}\r\n\r\n{reason}",
                text(self.zh, "Existing file:", "现有文件："),
                display(&alert.old_file.path),
                alert.old_file.size / (1024 * 1024),
                version.map_or(String::new(), |v| format!(" · v{v}"))
            );
            if alert.old_age {
                body.push_str(&text(self.zh,"\r\n\r\nIts earliest filesystem date is over three months old. Keep or delete it yourself.","\r\n\r\n该文件最早的文件系统日期距今超过三个月。保留或删除由你决定。"));
            }
            set_text(self.popup_controls[1], &body);
            set_text(
                self.popup_controls[2],
                &text(self.zh, "Open Location", "打开旧文件位置"),
            );
            set_text(self.popup_controls[3], &text(self.zh, "Ignore", "忽略"));
        }
    }
    unsafe fn place_popup(&self) {
        let mut point: POINT = zeroed();
        GetCursorPos(&mut point);
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return;
        }
        let dpi = GetDpiForWindow(self.popup).max(96) as i32;
        let w = (480 * dpi / 96).min(info.rcWork.right - info.rcWork.left - 20);
        let h = (340 * dpi / 96).min(info.rcWork.bottom - info.rcWork.top - 20);
        SetWindowPos(
            self.popup,
            HWND_TOPMOST,
            info.rcWork.right - w - 10,
            info.rcWork.bottom - h - 10,
            w,
            h,
            SWP_NOACTIVATE,
        );
    }
    unsafe fn layout(&mut self) {
        let dpi = GetDpiForWindow(self.main).max(96) as i32;
        let unit = |x: i32| x * dpi / 96;
        let mut rect: RECT = zeroed();
        GetClientRect(self.main, &mut rect);
        let w = rect.right;
        let h = rect.bottom;
        let margin = unit(20);
        let gap = unit(10);
        let controls = &self.main_controls;
        move_control(controls[0], margin, margin, w - 2 * margin, unit(32));
        move_control(controls[1], margin, unit(60), w - 2 * margin, unit(40));
        let list_y = unit(106);
        let bottom = h - unit(110);
        move_control(
            controls[2],
            margin,
            list_y,
            w - 2 * margin,
            bottom - list_y - gap,
        );
        let button_w = (w - 2 * margin - 3 * gap) / 4;
        for i in 0..4 {
            move_control(
                controls[3 + i],
                margin + i as i32 * (button_w + gap),
                bottom,
                button_w,
                unit(34),
            );
        }
        move_control(
            controls[7],
            margin,
            bottom + unit(44),
            w - 2 * margin,
            unit(60),
        );
        let pdpi = GetDpiForWindow(self.popup).max(96) as i32;
        let pu = |x: i32| x * pdpi / 96;
        let mut pr: RECT = zeroed();
        GetClientRect(self.popup, &mut pr);
        let pw = pr.right;
        let ph = pr.bottom;
        move_control(self.popup_controls[0], pu(18), pu(16), pw - pu(36), pu(30));
        move_control(
            self.popup_controls[1],
            pu(18),
            pu(54),
            pw - pu(36),
            ph - pu(116),
        );
        let bw = (pw - pu(46)) / 2;
        move_control(self.popup_controls[2], pu(18), ph - pu(48), bw, pu(32));
        move_control(self.popup_controls[3], pu(28) + bw, ph - pu(48), bw, pu(32));
        let mut new_fonts = Vec::new();
        for (controls, dpi) in [(&self.main_controls, dpi), (&self.popup_controls, pdpi)] {
            for (index, hwnd) in controls.iter().enumerate() {
                let height = if index == 0 { 20 } else { 14 };
                let font = CreateFontW(
                    -height * dpi / 96,
                    0,
                    0,
                    0,
                    if index == 0 { 600 } else { 400 },
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_DEFAULT_PRECIS as u32,
                    CLIP_DEFAULT_PRECIS as u32,
                    CLEARTYPE_QUALITY as u32,
                    DEFAULT_PITCH as u32,
                    wide("Segoe UI").as_ptr(),
                );
                SendMessageW(*hwnd, WM_SETFONT, font as usize, 1);
                new_fonts.push(font);
            }
        }
        for font in self.fonts.drain(..) {
            DeleteObject(font);
        }
        self.fonts = new_fonts;
    }
}

unsafe fn choose_folder(owner: HWND) -> Option<PathBuf> {
    let mut info: BROWSEINFOW = zeroed();
    info.hwndOwner = owner;
    let title = wide("Select a folder to authorize / 选择允许建立索引的文件夹");
    info.lpszTitle = title.as_ptr();
    info.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    let id = SHBrowseForFolderW(&info);
    if id.is_null() {
        return None;
    }
    let mut path = vec![0u16; 32768];
    let ok = SHGetPathFromIDListEx(id, path.as_mut_ptr(), path.len() as u32, 0);
    CoTaskMemFree(id.cast::<c_void>());
    if ok == 0 {
        None
    } else {
        let len = path.iter().position(|x| *x == 0).unwrap_or(path.len());
        Some(PathBuf::from(String::from_utf16_lossy(&path[..len])))
    }
}

unsafe fn reveal(path: &Path) -> Result<bool, String> {
    // Balance an explicit COM initialization for this operation. The settings
    // thread normally owns an STA already, in which case this returns S_FALSE.
    let init = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
    if init < 0 {
        return Err(format!("COM: 0x{:08X}", init as u32));
    }
    let result = reveal_initialized(path);
    CoUninitialize();
    result
}

unsafe fn reveal_initialized(path: &Path) -> Result<bool, String> {
    let converted = shell_wide(path)
        .ok_or_else(|| "Unsupported Shell path / Explorer 不支持此路径".to_string())?;
    let mut id = null_mut();
    let parsed = SHParseDisplayName(converted.as_ptr(), null_mut(), &mut id, 0, null_mut());
    let selection = if parsed >= 0 && !id.is_null() {
        let hr = SHOpenFolderAndSelectItems(id, 0, null(), 0);
        CoTaskMemFree(id.cast::<c_void>());
        hr
    } else {
        if !id.is_null() {
            CoTaskMemFree(id.cast::<c_void>());
        }
        if parsed >= 0 {
            E_UNEXPECTED
        } else {
            parsed
        }
    };
    if selection >= 0 {
        return Ok(true);
    }
    // If Shell item selection is unavailable, open only the containing
    // directory using the native Shell verb. Never launch the downloaded file.
    let parent = path
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| format!("Selection: 0x{:08X}; folder unavailable", selection as u32))?;
    let folder = shell_wide(parent)
        .ok_or_else(|| format!("Selection: 0x{:08X}; unsupported folder", selection as u32))?;
    let opened = ShellExecuteW(
        null_mut(),
        wide("open").as_ptr(),
        folder.as_ptr(),
        null(),
        null(),
        SW_SHOWNORMAL,
    ) as isize;
    if opened > 32 {
        Ok(false)
    } else {
        Err(format!(
            "Selection: 0x{:08X}; folder open: {}",
            selection as u32, opened
        ))
    }
}
