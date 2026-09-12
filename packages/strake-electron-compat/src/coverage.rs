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

/// The frozen top-50 Electron API surface, sorted by Electron name.
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
        strake: "Deferred: bind strake-shell native clipboard",
        status: Deferred("bind strake-shell native clipboard"),
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
        strake: "IpcBus::remove_all_listeners",
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
        electron: "screen.getPrimaryDisplay",
        strake: "Deferred: winit monitor bridge",
        status: Deferred("winit monitor bridge"),
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
        electron: "win.setProgressBar",
        strake: "Deferred: OS taskbar bridge",
        status: Deferred("OS taskbar bridge"),
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
fn coverage_freeze_has_fifty_sorted_unique_apis() {
    assert_eq!(TOP50.len(), 50, "the freeze is exactly the top 50");
    let names: Vec<_> = TOP50.iter().map(|entry| entry.electron).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "keep the freeze table sorted for review");
    sorted.dedup();
    assert_eq!(sorted.len(), 50, "no duplicate Electron APIs");
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
