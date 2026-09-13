//! Spec freeze: the top-50 Electron API surface (issue #21, step 1).
//!
//! Every entry maps one Electron API to its Strake counterpart plus an
//! honest support status. The freeze is CI-enforced (see tests below):
//! promoting a `Deferred` entry means implementing it, not editing prose.

/// Implementation status of one Electron API in the shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportStatus {
    /// Maps directly onto an existing Strake primitive, no compat logic.
    Native,
    /// Implemented by the compatibility core in this crate.
    Shimmed,
    /// Explicitly out of MVP scope; the string names the blocker.
    Deferred(&'static str),
}

/// One frozen Electron → Strake mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiEntry {
    /// Electron API, e.g. `"app.quit"` or `"ipcRenderer.invoke"`.
    pub electron: &'static str,
    /// Strake counterpart, e.g. `"App::quit"`, or the blocker for deferred APIs.
    pub strake: &'static str,
    /// MVP support status.
    pub status: SupportStatus,
}

use SupportStatus::{Deferred, Native, Shimmed};

/// The frozen Electron API surface, sorted by Electron name.
///
/// Seeded as the top 50 for the Day-1 shim (issue #21); slice PRs grow the
/// freeze by promoting Deferred entries or recording explicit new decisions
/// (issues #90–#96, #92–#95 here), and the count test below pins the exact
/// length so every addition is a reviewed diff.
pub const TOP50: &[ApiEntry] = &[
    ApiEntry {
        electron: "BrowserWindow constructor",
        strake: "WindowManager::create",
        status: Shimmed,
    },
    ApiEntry {
        electron: "BrowserWindow webPreferences",
        strake: "Deferred: needs #18 N-API preload sandbox",
        status: Deferred("needs #18 N-API preload sandbox"),
    },
    ApiEntry {
        electron: "Menu.buildFromTemplate",
        strake: "Deferred: needs #12 native menus",
        status: Deferred("needs #12 native menus"),
    },
    ApiEntry {
        electron: "Menu.setApplicationMenu",
        strake: "Deferred: needs #12 native menus",
        status: Deferred("needs #12 native menus"),
    },
    ApiEntry {
        electron: "MenuItem roles",
        strake: "Deferred: needs #12 native menus",
        status: Deferred("needs #12 native menus"),
    },
    ApiEntry {
        electron: "Notification",
        strake: "NotificationCenter::notify + vibey renderer binding",
        status: Shimmed,
    },
    ApiEntry {
        electron: "Tray constructor",
        strake: "Deferred: needs #12 native tray",
        status: Deferred("needs #12 native tray"),
    },
    ApiEntry {
        electron: "Tray.setContextMenu",
        strake: "Deferred: needs #12 native tray",
        status: Deferred("needs #12 native tray"),
    },
    ApiEntry {
        electron: "app.getName",
        strake: "App::name",
        status: Native,
    },
    ApiEntry {
        electron: "app.getPath",
        strake: "App::get_path/set_path",
        status: Shimmed,
    },
    ApiEntry {
        electron: "app.getVersion",
        strake: "App::version",
        status: Native,
    },
    ApiEntry {
        electron: "app.lifecycle events",
        strake: "App::on(Ready/WindowAllClosed/Activate/BeforeQuit)",
        status: Shimmed,
    },
    ApiEntry {
        electron: "app.quit",
        strake: "App::quit",
        status: Shimmed,
    },
    ApiEntry {
        electron: "app.setAsDefaultProtocolClient",
        strake: "Deferred: OS protocol registration",
        status: Deferred("OS protocol registration"),
    },
    ApiEntry {
        electron: "app.whenReady",
        strake: "App::mark_ready/on(Ready)",
        status: Shimmed,
    },
    ApiEntry {
        electron: "clipboard.readText/writeText",
        strake: "Clipboard::read_text/write_text",
        status: Shimmed,
    },
    ApiEntry {
        electron: "dialog.showMessageBox",
        strake: "Deferred: needs #12 native dialogs",
        status: Deferred("needs #12 native dialogs"),
    },
    ApiEntry {
        electron: "dialog.showOpenDialog",
        strake: "Deferred: needs #12 native dialogs",
        status: Deferred("needs #12 native dialogs"),
    },
    ApiEntry {
        electron: "dialog.showSaveDialog",
        strake: "Deferred: needs #12 native dialogs",
        status: Deferred("needs #12 native dialogs"),
    },
    ApiEntry {
        electron: "globalShortcut.register",
        strake: "Deferred: OS global hotkeys",
        status: Deferred("OS global hotkeys"),
    },
    ApiEntry {
        electron: "ipcMain.handle",
        strake: "IpcBus::handle",
        status: Shimmed,
    },
    ApiEntry {
        electron: "ipcMain.on",
        strake: "IpcBus::on",
        status: Shimmed,
    },
    ApiEntry {
        electron: "ipcRenderer.invoke",
        strake: "IpcBus::invoke",
        status: Shimmed,
    },
    ApiEntry {
        electron: "ipcRenderer.on",
        strake: "IpcBus::on",
        status: Shimmed,
    },
    ApiEntry {
        electron: "ipcRenderer.removeListener",
        strake: "IpcBus::on -> ListenerId + remove_listener",
        status: Shimmed,
    },
    ApiEntry {
        electron: "ipcRenderer.send",
        strake: "IpcBus::send",
        status: Shimmed,
    },
    ApiEntry {
        electron: "nativeTheme.on(updated)",
        strake: "Deferred: needs #12 theme bridge",
        status: Deferred("needs #12 theme bridge"),
    },
    ApiEntry {
        electron: "nativeTheme.shouldUseDarkColors",
        strake: "Deferred: needs #12 theme bridge",
        status: Deferred("needs #12 theme bridge"),
    },
    ApiEntry {
        electron: "powerMonitor",
        strake: "PowerHub/PowerMonitor::on + inject probe",
        status: Shimmed,
    },
    ApiEntry {
        electron: "powerSaveBlocker",
        strake: "PowerSaveBlocker::start/stop/isStarted",
        status: Shimmed,
    },
    ApiEntry {
        electron: "safeStorage",
        strake: "SafeStorage::encrypt_string/decrypt_string",
        status: Shimmed,
    },
    ApiEntry {
        electron: "screen.getAllDisplays",
        strake: "Screen::get_all_displays",
        status: Shimmed,
    },
    ApiEntry {
        electron: "screen.getDisplayMatching",
        strake: "Screen::get_display_matching",
        status: Shimmed,
    },
    ApiEntry {
        electron: "screen.getDisplayNearestPoint",
        strake: "Screen::get_display_nearest_point",
        status: Shimmed,
    },
    ApiEntry {
        electron: "screen.getPrimaryDisplay",
        strake: "Screen::get_primary_display",
        status: Shimmed,
    },
    ApiEntry {
        electron: "shell.openExternal",
        strake: "Deferred: needs #12 OS integration",
        status: Deferred("needs #12 OS integration"),
    },
    ApiEntry {
        electron: "shell.showItemInFolder",
        strake: "Deferred: needs #12 OS integration",
        status: Deferred("needs #12 OS integration"),
    },
    ApiEntry {
        electron: "win.blur",
        strake: "WindowManager::blur",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.close",
        strake: "WindowManager::close",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.focus",
        strake: "WindowManager::focus",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.getBounds",
        strake: "WindowManager::get_bounds",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.hide",
        strake: "WindowManager::hide",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.isMaximized",
        strake: "BrowserWindow::is_maximized",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.isMinimized",
        strake: "BrowserWindow::is_minimized",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.isVisible",
        strake: "WindowManager::is_visible",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.loadFile",
        strake: "WebContents::load_file",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.loadURL",
        strake: "WebContents::load_url",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.maximize",
        strake: "WindowManager::maximize",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.minimize",
        strake: "WindowManager::minimize",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.on(closed)",
        strake: "WindowManager::on_closed",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.restore",
        strake: "WindowManager::restore",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.setAlwaysOnTop",
        strake: "WindowManager::set_always_on_top",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.setBounds",
        strake: "WindowManager::set_bounds",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.setMenuBarVisibility",
        strake: "Deferred: needs #12 native menus",
        status: Deferred("needs #12 native menus"),
    },
    ApiEntry {
        electron: "win.setProgressBar",
        strake: "Deferred: OS taskbar bridge",
        status: Deferred("OS taskbar bridge"),
    },
    ApiEntry {
        electron: "win.setResizable",
        strake: "WindowManager::set_resizable",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.setTitle",
        strake: "WindowManager::set_title",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.show",
        strake: "WindowManager::show",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.webContents.executeJavaScript",
        strake: "Deferred: renderer JS-engine binding",
        status: Deferred("renderer JS-engine binding"),
    },
    ApiEntry {
        electron: "win.webContents.getTitle",
        strake: "WebContents::get_title",
        status: Shimmed,
    },
    ApiEntry {
        electron: "win.webContents.openDevTools",
        strake: "Deferred: devtools UI",
        status: Deferred("devtools UI"),
    },
    ApiEntry {
        electron: "win.webContents.print",
        strake: "Deferred: needs #12 shell printing",
        status: Deferred("needs #12 shell printing"),
    },
    ApiEntry {
        electron: "win.webContents.printToPDF",
        strake: "Deferred: needs #12 shell printing",
        status: Deferred("needs #12 shell printing"),
    },
    ApiEntry {
        electron: "win.webContents.send",
        strake: "WebContents::send",
        status: Shimmed,
    },
];

/// The MVP surface: every API the Day-1 shim must serve.
#[cfg(test)]
const MVP_MUST: &[&str] = &[
    "BrowserWindow constructor",
    "app.getPath",
    "app.lifecycle events",
    "app.quit",
    "app.whenReady",
    "ipcMain.handle",
    "ipcMain.on",
    "ipcRenderer.invoke",
    "ipcRenderer.on",
    "ipcRenderer.removeListener",
    "ipcRenderer.send",
    "win.close",
    "win.hide",
    "win.loadFile",
    "win.loadURL",
    "win.show",
    "win.webContents.send",
];

#[test]
fn coverage_freeze_is_sorted_unique_apis() {
    // Seeded at 50 for the Day-1 shim; issues #90/#96 add nine entries
    // (window geometry, webContents title, screen enumeration, plus the
    // setMenuBarVisibility deferral decision); issue #84 adds win.on(closed).
    // Issues #92–#95 add four entries (Notification, powerMonitor,
    // powerSaveBlocker, safeStorage) and promote clipboard.readText/writeText
    // to Shimmed.
    assert_eq!(TOP50.len(), 64, "freeze grows only by reviewed diff");
    let names: Vec<_> = TOP50.iter().map(|entry| entry.electron).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "keep the freeze table sorted for review");
    sorted.dedup();
    assert_eq!(sorted.len(), 64, "no duplicate Electron APIs");
}

#[test]
fn mvp_surface_is_implemented() {
    for api in MVP_MUST {
        let entry = TOP50
            .iter()
            .find(|entry| entry.electron == *api)
            .unwrap_or_else(|| panic!("{api} missing from the freeze"));
        assert!(
            !matches!(entry.status, Deferred(_)),
            "{api} must be MVP-implemented, mapped to {}",
            entry.strake
        );
    }
}
