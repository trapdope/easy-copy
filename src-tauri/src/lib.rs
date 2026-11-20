use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    env, fs,
    io::Cursor,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const HISTORY_LIMIT: usize = 100;
const POLL_INTERVAL_MS: u64 = 650;
const PASTE_ACTIVATE_TIMEOUT_MS: u64 = 140;
const PASTE_SETTLE_DELAY_MS: u64 = 45;
const FOCUS_LOSS_GRACE_MS: u64 = 200;
const SELF_CLIPBOARD_WRITE_GRACE_MS: u64 = 1500;
const HISTORY_EVENT: &str = "clipboard-history-updated";
const PALETTE_OPENED_EVENT: &str = "palette-opened";
const SETTINGS_EVENT: &str = "app-settings-updated";
const SETTINGS_OPENED_EVENT: &str = "settings-opened";
const PNG_DATA_URL_PREFIX: &str = "data:image/png;base64,";
const PALETTE_MARGIN: f64 = 12.0;
const PALETTE_CURSOR_GAP: f64 = 12.0;
const DEFAULT_SHORTCUT: &str = "Cmd+Shift+V";
const LAUNCH_AGENT_LABEL: &str = "com.blue.easycopy";

static LAST_PALETTE_SHOWN_AT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum ClipboardItemKind {
    Text,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardItem {
    id: u64,
    kind: ClipboardItemKind,
    preview: String,
    text: Option<String>,
    char_count: Option<usize>,
    image_data_url: Option<String>,
    width: Option<usize>,
    height: Option<usize>,
    byte_count: Option<usize>,
    source_app: Option<String>,
    created_at: u64,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyClipboardItem {
    id: u64,
    text: String,
    preview: String,
    char_count: usize,
    created_at: u64,
}

#[derive(Debug, Clone)]
enum ClipboardPayload {
    Text(String),
    Image {
        image_data_url: String,
        width: usize,
        height: usize,
        byte_count: usize,
    },
}

#[derive(Debug, Clone, Default)]
struct FocusTarget {
    pid: i32,
    process_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PasteResult {
    pasted: bool,
    needs_accessibility: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    launch_at_login: bool,
    shortcut: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            shortcut: DEFAULT_SHORTCUT.to_string(),
        }
    }
}

#[derive(Clone)]
struct AppSettingsStore {
    inner: Arc<AppSettingsStoreInner>,
}

struct AppSettingsStoreInner {
    settings: Mutex<AppSettings>,
    file_path: PathBuf,
}

impl AppSettingsStore {
    fn load(file_path: PathBuf) -> Self {
        let mut settings = load_app_settings(&file_path);

        settings.shortcut = normalize_shortcut_string(&settings.shortcut)
            .unwrap_or_else(|_| DEFAULT_SHORTCUT.to_string());
        settings.launch_at_login = is_launch_at_login_enabled();

        Self {
            inner: Arc::new(AppSettingsStoreInner {
                settings: Mutex::new(settings),
                file_path,
            }),
        }
    }

    fn snapshot(&self) -> AppSettings {
        self.inner.settings.lock().unwrap().clone()
    }

    fn replace(&self, settings: AppSettings) -> Result<(), String> {
        self.persist(&settings)?;
        *self.inner.settings.lock().unwrap() = settings;
        Ok(())
    }

    fn sync_launch_at_login(&self) -> AppSettings {
        let mut settings = self.snapshot();
        let launch_at_login = is_launch_at_login_enabled();

        if settings.launch_at_login != launch_at_login {
            settings.launch_at_login = launch_at_login;
            let _ = self.replace(settings.clone());
        }

        settings
    }

    fn persist(&self, settings: &AppSettings) -> Result<(), String> {
        if let Some(parent) = self.inner.file_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }

        let content = serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
        fs::write(&self.inner.file_path, content).map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct ShortcutRegistration {
    registered: Arc<Mutex<Option<String>>>,
}

impl ShortcutRegistration {
    fn new(registered: Option<String>) -> Self {
        Self {
            registered: Arc::new(Mutex::new(registered)),
        }
    }

    fn get(&self) -> Option<String> {
        self.registered.lock().unwrap().clone()
    }

    fn set(&self, shortcut: Option<String>) {
        *self.registered.lock().unwrap() = shortcut;
    }
}

#[derive(Clone)]
struct ClipboardHistory {
    inner: Arc<ClipboardHistoryInner>,
}

struct ClipboardHistoryInner {
    items: Mutex<Vec<ClipboardItem>>,
    next_id: AtomicU64,
    file_path: PathBuf,
    focus_target: Mutex<Option<FocusTarget>>,
    ignore_source_app_until: AtomicU64,
}

impl ClipboardHistory {
    fn load(file_path: PathBuf) -> Self {
        let mut items = load_history_items(&file_path);
        trim_history(&mut items);
        let next_id = items.iter().map(|item| item.id).max().unwrap_or(0) + 1;

        Self {
            inner: Arc::new(ClipboardHistoryInner {
                items: Mutex::new(items),
                next_id: AtomicU64::new(next_id),
                file_path,
                focus_target: Mutex::new(None),
                ignore_source_app_until: AtomicU64::new(0),
            }),
        }
    }

    fn snapshot(&self) -> Vec<ClipboardItem> {
        self.inner.items.lock().unwrap().clone()
    }

    fn find(&self, id: u64) -> Option<ClipboardItem> {
        self.inner
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    fn mark_self_clipboard_write(&self) {
        self.inner.ignore_source_app_until.store(
            now_millis().saturating_add(SELF_CLIPBOARD_WRITE_GRACE_MS),
            Ordering::Relaxed,
        );
    }

    fn is_self_clipboard_write_active(&self) -> bool {
        now_millis() <= self.inner.ignore_source_app_until.load(Ordering::Relaxed)
    }

    fn promote(&self, id: u64) -> Option<Vec<ClipboardItem>> {
        let mut items = self.inner.items.lock().unwrap();
        let index = items.iter().position(|item| item.id == id)?;

        if index == 0 {
            return None;
        }

        let mut item = items.remove(index);
        item.created_at = now_millis();
        items.insert(0, item);

        let snapshot = items.clone();
        drop(items);
        self.persist(&snapshot);
        Some(snapshot)
    }

    fn delete(&self, id: u64) -> Option<Vec<ClipboardItem>> {
        let mut items = self.inner.items.lock().unwrap();
        let original_len = items.len();
        items.retain(|item| item.id != id);

        if items.len() == original_len {
            return None;
        }

        let snapshot = items.clone();
        drop(items);
        self.persist(&snapshot);
        Some(snapshot)
    }

    fn set_locked(&self, id: u64, locked: bool) -> Option<Vec<ClipboardItem>> {
        let mut items = self.inner.items.lock().unwrap();
        let item = items.iter_mut().find(|item| item.id == id)?;
        item.locked = locked;
        trim_history(&mut items);
        let snapshot = items.clone();
        drop(items);
        self.persist(&snapshot);
        Some(snapshot)
    }

    fn set_note(&self, id: u64, note: String) -> Option<Vec<ClipboardItem>> {
        let mut items = self.inner.items.lock().unwrap();
        let item = items.iter_mut().find(|item| item.id == id)?;
        let note = note.trim();
        item.note = (!note.is_empty()).then(|| note.to_string());
        let snapshot = items.clone();
        drop(items);
        self.persist(&snapshot);
        Some(snapshot)
    }

    fn push_payload(
        &self,
        payload: ClipboardPayload,
        source_app: Option<String>,
    ) -> Option<Vec<ClipboardItem>> {
        let mut items = self.inner.items.lock().unwrap();
        let existing_source_app = items
            .iter()
            .find(|item| item_matches_payload(item, &payload))
            .and_then(|item| item.source_app.clone());
        let effective_source_app = if self.is_self_clipboard_write_active() {
            existing_source_app
        } else {
            source_app.or(existing_source_app)
        };

        if items
            .first()
            .is_some_and(|item| item_matches_payload(item, &payload))
        {
            if items[0].source_app.is_none() {
                if let Some(source_app) = effective_source_app {
                    items[0].source_app = Some(source_app);
                    let snapshot = items.clone();
                    drop(items);
                    self.persist(&snapshot);
                    return Some(snapshot);
                }
            }

            return None;
        }

        let existing_index = items.iter().position(|item| item_matches_payload(item, &payload));
        let item = if let Some(index) = existing_index {
            let mut item = items.remove(index);
            if effective_source_app.is_some() {
                item.source_app = effective_source_app;
            }
            item.created_at = now_millis();
            item
        } else {
            clipboard_item_from_payload(
                self.inner.next_id.fetch_add(1, Ordering::Relaxed),
                payload,
                effective_source_app,
            )
        };

        items.insert(0, item);
        trim_history(&mut items);

        let snapshot = items.clone();
        drop(items);
        self.persist(&snapshot);

        Some(snapshot)
    }

    fn persist(&self, items: &[ClipboardItem]) {
        if let Some(parent) = self.inner.file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Ok(content) = serde_json::to_string_pretty(items) {
            let _ = fs::write(&self.inner.file_path, content);
        }
    }

    fn set_focus_target(&self, target: Option<FocusTarget>) {
        *self.inner.focus_target.lock().unwrap() = target;
    }

    fn focus_target(&self) -> Option<FocusTarget> {
        self.inner.focus_target.lock().unwrap().clone()
    }
}

#[tauri::command]
fn get_clipboard_history(history: State<'_, ClipboardHistory>) -> Vec<ClipboardItem> {
    history.snapshot()
}

#[tauri::command]
fn get_app_settings(settings: State<'_, AppSettingsStore>) -> AppSettings {
    settings.sync_launch_at_login()
}

#[tauri::command]
fn update_app_settings(
    app: AppHandle,
    settings_store: State<'_, AppSettingsStore>,
    shortcut_registration: State<'_, ShortcutRegistration>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    let old_settings = settings_store.snapshot();
    let mut next_settings = settings;
    next_settings.shortcut = normalize_shortcut_string(&next_settings.shortcut)?;

    if next_settings.shortcut != old_settings.shortcut {
        register_updated_palette_shortcut(
            &app,
            &shortcut_registration,
            &old_settings.shortcut,
            &next_settings.shortcut,
        )?;
    }

    if next_settings.launch_at_login != old_settings.launch_at_login {
        if let Err(error) = set_launch_at_login_enabled(next_settings.launch_at_login) {
            if next_settings.shortcut != old_settings.shortcut {
                rollback_palette_shortcut(
                    &app,
                    &shortcut_registration,
                    &old_settings.shortcut,
                    &next_settings.shortcut,
                );
            }

            return Err(error);
        }
    }

    settings_store.replace(next_settings.clone())?;
    app.emit(SETTINGS_EVENT, next_settings.clone())
        .map_err(|error| error.to_string())?;

    Ok(next_settings)
}

#[tauri::command]
fn show_palette(app: AppHandle) -> Result<(), String> {
    show_palette_window(&app).map_err(|error| error.to_string())
}

#[tauri::command]
fn hide_palette(app: AppHandle) -> Result<(), String> {
    hide_palette_window(&app).map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_clipboard_item(
    app: AppHandle,
    history: State<'_, ClipboardHistory>,
    id: u64,
) -> Result<(), String> {
    let snapshot = history
        .delete(id)
        .ok_or_else(|| "Clipboard item no longer exists.".to_string())?;

    app.emit(HISTORY_EVENT, snapshot)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_clipboard_item_locked(
    app: AppHandle,
    history: State<'_, ClipboardHistory>,
    id: u64,
    locked: bool,
) -> Result<(), String> {
    let snapshot = history
        .set_locked(id, locked)
        .ok_or_else(|| "Clipboard item no longer exists.".to_string())?;
    app.emit(HISTORY_EVENT, snapshot)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_clipboard_item_note(
    app: AppHandle,
    history: State<'_, ClipboardHistory>,
    id: u64,
    note: String,
) -> Result<(), String> {
    let snapshot = history
        .set_note(id, note)
        .ok_or_else(|| "Clipboard item no longer exists.".to_string())?;
    app.emit(HISTORY_EVENT, snapshot)
        .map_err(|error| error.to_string())
}

#[tauri::command(async)]
fn paste_clipboard_item(
    app: AppHandle,
    history: State<'_, ClipboardHistory>,
    id: u64,
) -> Result<PasteResult, String> {
    let item = history
        .find(id)
        .ok_or_else(|| "剪贴板记录已不存在。".to_string())?;

    history.mark_self_clipboard_write();
    write_clipboard_item(&item)?;

    if let Some(snapshot) = history.promote(id) {
        let _ = app.emit(HISTORY_EVENT, snapshot);
    }

    if !is_accessibility_trusted() {
        let _ = prompt_accessibility_permission();
        return Ok(PasteResult {
            pasted: false,
            needs_accessibility: true,
        });
    }

    let target = history.focus_target().or_else(capture_focus_target);

    hide_palette_and_activate_target(&app, target.as_ref())
        .map_err(|error| error.to_string())?;

    if let Some(target) = target.as_ref() {
        wait_for_target_frontmost(target, PASTE_ACTIVATE_TIMEOUT_MS);
    }
    thread::sleep(Duration::from_millis(PASTE_SETTLE_DELAY_MS));

    paste_into_focus_target(target.as_ref())?;

    Ok(PasteResult {
        pasted: true,
        needs_accessibility: false,
    })
}

#[tauri::command]
fn check_accessibility_permission() -> bool {
    is_accessibility_trusted()
}

#[tauri::command]
fn request_accessibility_permission() -> Result<bool, String> {
    let trusted = prompt_accessibility_permission();
    if !trusted {
        let _ = open_accessibility_settings();
    }
    Ok(trusted)
}

#[tauri::command]
fn open_accessibility_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = prompt_accessibility_permission();
        Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn()
            .map_err(|error| error.to_string())?;
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("Accessibility settings are only available on macOS.".to_string())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        let _ = show_palette_window_from_shortcut(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory)
                .map_err(|error| error.to_string())?;

            let app_handle = app.handle().clone();
            let history = ClipboardHistory::load(history_file_path(&app_handle)?);
            let settings = AppSettingsStore::load(settings_file_path(&app_handle)?);
            let registered_shortcut =
                register_initial_palette_shortcut(app.handle(), &settings.snapshot().shortcut);
            app.manage(history.clone());
            app.manage(settings);
            app.manage(ShortcutRegistration::new(registered_shortcut));

            setup_palette_window(app.handle());
            setup_tray(app.handle())?;

            spawn_clipboard_watcher(app_handle, history);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_clipboard_history,
            get_app_settings,
            update_app_settings,
            show_palette,
            hide_palette,
            delete_clipboard_item,
            set_clipboard_item_locked,
            set_clipboard_item_note,
            paste_clipboard_item,
            check_accessibility_permission,
            request_accessibility_permission,
            open_accessibility_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn setup_palette_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window.set_always_on_top(true);
            let _ = window.set_visible_on_all_workspaces(true);
        }
        configure_palette_window_for_fullscreen(&window);

        let app_for_events = app.clone();
        window.on_window_event(move |event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
            }
            WindowEvent::Focused(false) => {
                let shown_at = LAST_PALETTE_SHOWN_AT.load(Ordering::Relaxed);
                if now_millis().saturating_sub(shown_at) > FOCUS_LOSS_GRACE_MS {
                    let _ = hide_palette_window(&app_for_events);
                }
            }
            _ => {}
        });
    }
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示剪贴板", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "选项", true, None::<&str>)?;
    let accessibility =
        MenuItem::with_id(app, "accessibility", "辅助功能权限", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 EasyCopy", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &settings, &accessibility, &quit])?;

    let mut builder = TrayIconBuilder::with_id("easycopy-tray")
        .tooltip("EasyCopy")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                let _ = show_palette_window(app);
            }
            "settings" => {
                let _ = show_settings_window(app);
            }
            "accessibility" => {
                let _ = open_accessibility_settings();
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = show_palette_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }

    builder.build(app)?;
    Ok(())
}

fn spawn_clipboard_watcher(app: AppHandle, history: ClipboardHistory) {
    thread::spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(_) => return,
        };
        let mut last_image_signature = String::new();
        let mut last_text = String::new();

        loop {
            if !is_palette_window_visible(&app) {
                if let Some(target) = capture_focus_target() {
                    history.set_focus_target(Some(target));
                }
            }

            if let Ok(image) = clipboard.get_image() {
                let width = image.width;
                let height = image.height;
                let rgba = image.bytes.into_owned();
                let signature = image_signature(width, height, &rgba);

                if signature != last_image_signature {
                    last_image_signature = signature;
                    if let Ok(payload) = image_payload_from_rgba(width, height, rgba) {
                        emit_history_if_changed(
                            &app,
                            &history,
                            payload,
                            capture_clipboard_source_app(),
                        );
                    }
                }
            } else if let Ok(text) = clipboard.get_text() {
                if text != last_text {
                    last_text = text.clone();
                    if let Some(payload) = text_payload(text) {
                        emit_history_if_changed(
                            &app,
                            &history,
                            payload,
                            capture_clipboard_source_app(),
                        );
                    }
                }
            }

            thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    });
}

fn emit_history_if_changed(
    app: &AppHandle,
    history: &ClipboardHistory,
    payload: ClipboardPayload,
    source_app: Option<String>,
) {
    if let Some(snapshot) = history.push_payload(payload, source_app) {
        let _ = app.emit(HISTORY_EVENT, snapshot);
    }
}

fn is_palette_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

fn run_on_main_thread_sync<F, R>(app: &AppHandle, f: F) -> tauri::Result<R>
where
    F: FnOnce() -> tauri::Result<R> + Send + 'static,
    R: Send + 'static,
{
    #[cfg(target_os = "macos")]
    if objc2_foundation::MainThreadMarker::new().is_some() {
        return f();
    }

    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })?;
    rx.recv()
        .map_err(|_| tauri::Error::FailedToReceiveMessage)?
}

fn show_palette_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(focus_target) = capture_focus_target() {
        if let Some(history) = app.try_state::<ClipboardHistory>() {
            history.set_focus_target(Some(focus_target));
        }
    }

    let app_for_main = app.clone();
    run_on_main_thread_sync(app, move || {
        if let Some(window) = app_for_main.get_webview_window("main") {
            LAST_PALETTE_SHOWN_AT.store(now_millis(), Ordering::Relaxed);
            #[cfg(not(target_os = "macos"))]
            {
                let _ = window.set_always_on_top(true);
                let _ = window.set_visible_on_all_workspaces(true);
            }
            configure_palette_window_for_fullscreen(&window);
            place_palette_near_cursor(&app_for_main, &window);
            show_palette_interactively(&window)?;
            let _ = window.emit(PALETTE_OPENED_EVENT, ());
        }
        Ok(())
    })
}

fn hide_palette_window(app: &AppHandle) -> tauri::Result<()> {
    let app_for_main = app.clone();
    run_on_main_thread_sync(app, move || {
        if let Some(window) = app_for_main.get_webview_window("main") {
            #[cfg(target_os = "macos")]
            {
                use objc2_app_kit::NSWindow;
                if let Ok(raw_window) = window.ns_window() {
                    if let Some(native_window) = unsafe { raw_window.cast::<NSWindow>().as_ref() } {
                        native_window.orderOut(None);
                        return Ok(());
                    }
                }
            }
            window.hide()?;
        }
        Ok(())
    })
}

fn hide_palette_and_activate_target(
    app: &AppHandle,
    target: Option<&FocusTarget>,
) -> tauri::Result<()> {
    let app_for_main = app.clone();
    let target_cloned = target.cloned();
    run_on_main_thread_sync(app, move || {
        if let Some(window) = app_for_main.get_webview_window("main") {
            #[cfg(target_os = "macos")]
            {
                use objc2_app_kit::NSWindow;
                if let Ok(raw_window) = window.ns_window() {
                    if let Some(native_window) = unsafe { raw_window.cast::<NSWindow>().as_ref() } {
                        native_window.orderOut(None);
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                window.hide()?;
            }
        }
        if let Some(target) = target_cloned.as_ref() {
            activate_focus_target(target);
        }
        Ok(())
    })
}

fn show_palette_window_from_shortcut(app: &AppHandle) -> tauri::Result<()> {
    if is_palette_window_visible(app) {
        return hide_palette_window(app);
    }

    show_palette_window(app)
}

fn show_settings_window(app: &AppHandle) -> tauri::Result<()> {
    show_palette_window(app)?;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit(SETTINGS_OPENED_EVENT, ());
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn show_palette_interactively(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    use objc2_app_kit::{NSApplication, NSWindow};
    use objc2_foundation::MainThreadMarker;

    let native_window = window.ns_window()?;
    let Some(native_window) = (unsafe { native_window.cast::<NSWindow>().as_ref() }) else {
        return Ok(());
    };

    let Some(main_thread) = MainThreadMarker::new() else {
        return Ok(());
    };
    NSApplication::sharedApplication(main_thread).activateIgnoringOtherApps(true);
    native_window.makeKeyAndOrderFront(None);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn show_palette_interactively(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    window.show()
}

fn place_palette_near_cursor(app: &AppHandle, window: &tauri::WebviewWindow) {
    place_palette_near_cursor_with_tauri(app, window);
}

#[cfg(target_os = "macos")]
fn configure_palette_window_for_fullscreen(window: &tauri::WebviewWindow) {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior, NSStatusWindowLevel};

    let Ok(native_window) = window.ns_window() else {
        return;
    };
    let Some(native_window) = (unsafe { native_window.cast::<NSWindow>().as_ref() }) else {
        return;
    };

    let behavior = native_window.collectionBehavior()
        | NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::FullScreenAuxiliary;
    native_window.setCollectionBehavior(behavior);
    native_window.setLevel(NSStatusWindowLevel);
    native_window.setHidesOnDeactivate(false);
}

#[cfg(not(target_os = "macos"))]
fn configure_palette_window_for_fullscreen(_window: &tauri::WebviewWindow) {
}

#[cfg(target_os = "macos")]
fn place_palette_near_cursor_with_tauri(_app: &AppHandle, window: &tauri::WebviewWindow) {
    use objc2_app_kit::{NSEvent, NSScreen, NSWindow};
    use objc2_foundation::{MainThreadMarker, NSPoint};

    let Ok(native_window) = window.ns_window() else {
        return;
    };
    let Some(native_window) = (unsafe { native_window.cast::<NSWindow>().as_ref() }) else {
        return;
    };
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };

    let cursor = NSEvent::mouseLocation();
    let screens = NSScreen::screens(main_thread);
    let screen = screens
        .iter()
        .find(|screen| {
            let frame = screen.frame();
            cursor.x >= frame.origin.x
                && cursor.x <= frame.origin.x + frame.size.width
                && cursor.y >= frame.origin.y
                && cursor.y <= frame.origin.y + frame.size.height
        })
        .or_else(|| NSScreen::mainScreen(main_thread))
        .or_else(|| screens.iter().next());

    let Some(screen) = screen else {
        return;
    };

    let visible = screen.visibleFrame();
    let window_frame = native_window.frame();
    let width = if window_frame.size.width > 0.0 {
        window_frame.size.width
    } else {
        380.0
    };
    let height = if window_frame.size.height > 0.0 {
        window_frame.size.height
    } else {
        500.0
    };
    let margin = PALETTE_MARGIN;
    let gap = PALETTE_CURSOR_GAP;
    let min_x = visible.origin.x + margin;
    let max_x = (visible.origin.x + visible.size.width - width - margin).max(min_x);
    let min_y = visible.origin.y + margin;
    let max_y = (visible.origin.y + visible.size.height - height - margin).max(min_y);
    let right_x = cursor.x + gap;
    let below_y = cursor.y - height - gap;
    let preferred_x = if right_x + width <= visible.origin.x + visible.size.width - margin {
        right_x
    } else {
        cursor.x - width - gap
    };
    let preferred_y = if below_y >= min_y {
        below_y
    } else {
        cursor.y + gap
    };
    let x = preferred_x.max(min_x).min(max_x);
    let y = preferred_y.max(min_y).min(max_y);

    native_window.setFrameOrigin(NSPoint::new(x, y));
}

#[cfg(not(target_os = "macos"))]
fn place_palette_near_cursor_with_tauri(app: &AppHandle, window: &tauri::WebviewWindow) {
    let Ok(cursor) = app.cursor_position() else {
        let _ = window.center();
        return;
    };
    let Ok(Some(monitor)) = app.monitor_from_point(cursor.x, cursor.y) else {
        let _ = window.center();
        return;
    };
    let Ok(window_size) = window.outer_size() else {
        let _ = window.center();
        return;
    };

    let monitor_pos = monitor.position();
    let monitor_size = monitor.size();
    let scale_factor = monitor.scale_factor();
    let margin = (PALETTE_MARGIN * scale_factor).round() as i32;
    let gap = (PALETTE_CURSOR_GAP * scale_factor).round() as i32;
    let window_width = window_size.width as i32;
    let window_height = window_size.height as i32;
    let min_x = monitor_pos.x + margin;
    let min_y = monitor_pos.y + margin;
    let max_x = monitor_pos.x + monitor_size.width as i32 - window_width - margin;
    let max_y = monitor_pos.y + monitor_size.height as i32 - window_height - margin;
    let cursor_x = cursor.x.round() as i32;
    let cursor_y = cursor.y.round() as i32;
    let preferred_x = if cursor_x + gap <= max_x {
        cursor_x + gap
    } else {
        cursor_x - window_width - gap
    };
    let preferred_y = if cursor_y + gap <= max_y {
        cursor_y + gap
    } else {
        cursor_y - window_height - gap
    };
    let x = clamp_i32(preferred_x, min_x, max_x);
    let y = clamp_i32(preferred_y, min_y, max_y);

    let _ = window.set_position(tauri::PhysicalPosition { x, y });
}

#[cfg(not(target_os = "macos"))]
fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
    if min > max {
        min
    } else {
        value.max(min).min(max)
    }
}

fn trim_history(items: &mut Vec<ClipboardItem>) {
    let locked_count = items.iter().filter(|item| item.locked).count();
    let unlocked_limit = HISTORY_LIMIT.saturating_sub(locked_count);
    let mut unlocked_kept = 0;
    items.retain(|item| {
        if item.locked {
            true
        } else if unlocked_kept < unlocked_limit {
            unlocked_kept += 1;
            true
        } else {
            false
        }
    });
}

fn load_history_items(file_path: &PathBuf) -> Vec<ClipboardItem> {
    let Some(content) = fs::read_to_string(file_path).ok() else {
        return Vec::new();
    };

    if let Ok(items) = serde_json::from_str::<Vec<ClipboardItem>>(&content) {
        return items;
    }

    serde_json::from_str::<Vec<LegacyClipboardItem>>(&content)
        .map(|items| {
            items
                .into_iter()
                .map(|item| ClipboardItem {
                    id: item.id,
                    kind: ClipboardItemKind::Text,
                    preview: item.preview,
                    text: Some(item.text),
                    char_count: Some(item.char_count),
                    image_data_url: None,
                    width: None,
                    height: None,
                    byte_count: None,
                    source_app: None,
                    created_at: item.created_at,
                    locked: false,
                    note: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn load_app_settings(file_path: &PathBuf) -> AppSettings {
    let Some(content) = fs::read_to_string(file_path).ok() else {
        return AppSettings::default();
    };

    serde_json::from_str::<AppSettings>(&content).unwrap_or_default()
}

fn history_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("history.json"))
        .map_err(|error| error.to_string())
}

fn settings_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("settings.json"))
        .map_err(|error| error.to_string())
}

fn normalize_shortcut_string(value: &str) -> Result<String, String> {
    let shortcut = parse_palette_shortcut(value)?;
    Ok(shortcut_label(shortcut))
}

fn parse_palette_shortcut(value: &str) -> Result<Shortcut, String> {
    let shortcut_text = value.trim();

    if shortcut_text.is_empty() {
        return Err("快捷键不能为空。".to_string());
    }

    let shortcut = shortcut_text
        .parse::<Shortcut>()
        .map_err(|error| format!("无法识别快捷键：{error}"))?;

    let actionable_modifiers = tauri_plugin_global_shortcut::Modifiers::SUPER
        | tauri_plugin_global_shortcut::Modifiers::CONTROL
        | tauri_plugin_global_shortcut::Modifiers::ALT;

    if !shortcut.mods.intersects(actionable_modifiers) {
        return Err("快捷键至少需要包含 Cmd、Ctrl 或 Option。".to_string());
    }

    Ok(shortcut)
}

fn shortcut_label(shortcut: Shortcut) -> String {
    let mut parts = Vec::new();

    if shortcut
        .mods
        .contains(tauri_plugin_global_shortcut::Modifiers::SUPER)
    {
        parts.push("Cmd".to_string());
    }
    if shortcut
        .mods
        .contains(tauri_plugin_global_shortcut::Modifiers::CONTROL)
    {
        parts.push("Ctrl".to_string());
    }
    if shortcut
        .mods
        .contains(tauri_plugin_global_shortcut::Modifiers::ALT)
    {
        parts.push("Option".to_string());
    }
    if shortcut
        .mods
        .contains(tauri_plugin_global_shortcut::Modifiers::SHIFT)
    {
        parts.push("Shift".to_string());
    }

    parts.push(shortcut_key_label(shortcut.key));
    parts.join("+")
}

fn shortcut_key_label(key: tauri_plugin_global_shortcut::Code) -> String {
    let raw = key.to_string();

    if let Some(letter) = raw.strip_prefix("Key") {
        return letter.to_string();
    }

    if let Some(digit) = raw.strip_prefix("Digit") {
        return digit.to_string();
    }

    match raw.as_str() {
        "ArrowDown" => "Down".to_string(),
        "ArrowLeft" => "Left".to_string(),
        "ArrowRight" => "Right".to_string(),
        "ArrowUp" => "Up".to_string(),
        "Backquote" => "`".to_string(),
        "Backslash" => "\\".to_string(),
        "BracketLeft" => "[".to_string(),
        "BracketRight" => "]".to_string(),
        "Comma" => ",".to_string(),
        "Equal" => "=".to_string(),
        "Minus" => "-".to_string(),
        "Period" => ".".to_string(),
        "Quote" => "'".to_string(),
        "Semicolon" => ";".to_string(),
        "Slash" => "/".to_string(),
        _ => raw,
    }
}

fn register_initial_palette_shortcut(app: &AppHandle, shortcut: &str) -> Option<String> {
    let candidates = [shortcut, DEFAULT_SHORTCUT];
    let mut attempted = Vec::new();

    for candidate in candidates {
        let Ok(normalized) = normalize_shortcut_string(candidate) else {
            continue;
        };

        if attempted.iter().any(|item| item == &normalized) {
            continue;
        }

        attempted.push(normalized.clone());

        if register_palette_shortcut(app, &normalized).is_ok() {
            return Some(normalized);
        }
    }

    None
}

fn register_updated_palette_shortcut(
    app: &AppHandle,
    shortcut_registration: &ShortcutRegistration,
    old_shortcut: &str,
    new_shortcut: &str,
) -> Result<(), String> {
    let registered_shortcut = shortcut_registration
        .get()
        .unwrap_or_else(|| old_shortcut.to_string());

    unregister_palette_shortcut(app, &registered_shortcut);
    if let Err(error) = register_palette_shortcut(app, new_shortcut) {
        let _ = register_palette_shortcut(app, &registered_shortcut);
        return Err(error);
    }

    shortcut_registration.set(Some(new_shortcut.to_string()));
    Ok(())
}

fn rollback_palette_shortcut(
    app: &AppHandle,
    shortcut_registration: &ShortcutRegistration,
    old_shortcut: &str,
    new_shortcut: &str,
) {
    unregister_palette_shortcut(app, new_shortcut);

    if register_palette_shortcut(app, old_shortcut).is_ok() {
        shortcut_registration.set(Some(old_shortcut.to_string()));
    } else {
        shortcut_registration.set(None);
    }
}

fn register_palette_shortcut(app: &AppHandle, shortcut: &str) -> Result<(), String> {
    let shortcut = parse_palette_shortcut(shortcut)?;
    app.global_shortcut()
        .register(shortcut)
        .map_err(|error| format!("无法注册快捷键：{error}"))
}

fn unregister_palette_shortcut(app: &AppHandle, shortcut: &str) {
    if let Ok(shortcut) = parse_palette_shortcut(shortcut) {
        let _ = app.global_shortcut().unregister(shortcut);
    }
}

#[cfg(target_os = "macos")]
fn is_launch_at_login_enabled() -> bool {
    let Some(path) = launch_agent_path() else {
        return false;
    };

    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };

    let executable = launch_agent_executable();
    content.contains(executable.to_string_lossy().as_ref())
}

#[cfg(not(target_os = "macos"))]
fn is_launch_at_login_enabled() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn set_launch_at_login_enabled(enabled: bool) -> Result<(), String> {
    let path = launch_agent_path().ok_or_else(|| "无法定位 LaunchAgents 目录。".to_string())?;
    let domain = launch_agent_domain()?;

    if enabled {
        let executable = launch_agent_executable();
        let content = launch_agent_plist(&executable.to_string_lossy());

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }

        fs::write(&path, content).map_err(|error| error.to_string())?;

        // Replace an already loaded job so the new executable path takes effect now.
        let _ = Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(&domain)
            .arg(&path)
            .output();

        let output = Command::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(&domain)
            .arg(&path)
            .output()
            .map_err(|error| format!("无法加载开机启动项：{error}"))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "无法加载开机启动项：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    } else {
        let _ = Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(&domain)
            .arg(&path)
            .output();

        if path.exists() {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }

        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_executable() -> PathBuf {
    let installed = PathBuf::from("/Applications/EasyCopy.app/Contents/MacOS/easy-copy");
    if installed.exists() {
        installed
    } else {
        env::current_exe().unwrap_or_else(|_| installed)
    }
}

#[cfg(target_os = "macos")]
fn launch_agent_domain() -> Result<String, String> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|error| format!("无法读取当前用户 ID：{error}"))?;

    if !output.status.success() {
        return Err("无法读取当前用户 ID。".to_string());
    }

    let uid = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    Ok(format!("gui/{}", uid.trim()))
}

#[cfg(not(target_os = "macos"))]
fn set_launch_at_login_enabled(_enabled: bool) -> Result<(), String> {
    Err("开机启动设置目前仅支持 macOS。".to_string())
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join("Library/LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

#[cfg(target_os = "macos")]
fn launch_agent_plist(executable: &str) -> String {
    let executable = escape_xml(executable);

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

#[cfg(target_os = "macos")]
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn text_payload(text: String) -> Option<ClipboardPayload> {
    let normalized = normalize_clipboard_text(&text)?;
    Some(ClipboardPayload::Text(normalized))
}

fn normalize_clipboard_text(text: &str) -> Option<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim();

    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn image_payload_from_rgba(
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> Result<ClipboardPayload, String> {
    let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
        .ok_or_else(|| "Invalid RGBA image bytes from clipboard.".to_string())?;
    let dynamic_image = image::DynamicImage::ImageRgba8(image);
    let mut png = Cursor::new(Vec::new());

    dynamic_image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;

    let png_bytes = png.into_inner();
    let byte_count = png_bytes.len();
    let image_data_url = format!(
        "{}{}",
        PNG_DATA_URL_PREFIX,
        BASE64_STANDARD.encode(&png_bytes)
    );

    Ok(ClipboardPayload::Image {
        image_data_url,
        width,
        height,
        byte_count,
    })
}

fn clipboard_item_from_payload(
    id: u64,
    payload: ClipboardPayload,
    source_app: Option<String>,
) -> ClipboardItem {
    match payload {
        ClipboardPayload::Text(text) => ClipboardItem {
            id,
            kind: ClipboardItemKind::Text,
            preview: build_text_preview(&text),
            char_count: Some(text.chars().count()),
            text: Some(text),
            image_data_url: None,
            width: None,
            height: None,
            byte_count: None,
            source_app,
            created_at: now_millis(),
            locked: false,
            note: None,
        },
        ClipboardPayload::Image {
            image_data_url,
            width,
            height,
            byte_count,
        } => ClipboardItem {
            id,
            kind: ClipboardItemKind::Image,
            preview: format!("Image {}x{}", width, height),
            text: None,
            char_count: None,
            image_data_url: Some(image_data_url),
            width: Some(width),
            height: Some(height),
            byte_count: Some(byte_count),
            source_app,
            created_at: now_millis(),
            locked: false,
            note: None,
        },
    }
}

fn item_matches_payload(item: &ClipboardItem, payload: &ClipboardPayload) -> bool {
    match payload {
        ClipboardPayload::Text(text) => {
            item.kind == ClipboardItemKind::Text && item.text.as_deref() == Some(text.as_str())
        }
        ClipboardPayload::Image { image_data_url, .. } => {
            item.kind == ClipboardItemKind::Image
                && item.image_data_url.as_deref() == Some(image_data_url.as_str())
        }
    }
}

fn build_text_preview(text: &str) -> String {
    const PREVIEW_LIMIT: usize = 160;

    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = single_line.chars().take(PREVIEW_LIMIT).collect::<String>();

    if single_line.chars().count() > PREVIEW_LIMIT {
        preview.push_str("...");
    }

    preview
}

fn write_clipboard_item(item: &ClipboardItem) -> Result<(), String> {
    match item.kind {
        ClipboardItemKind::Text => {
            let text = item
                .text
                .as_ref()
                .ok_or_else(|| "Text clipboard item is missing content.".to_string())?;
            write_clipboard_text(text)
        }
        ClipboardItemKind::Image => {
            let data_url = item
                .image_data_url
                .as_ref()
                .ok_or_else(|| "Image clipboard item is missing content.".to_string())?;
            write_clipboard_image(data_url)
        }
    }
}

fn write_clipboard_text(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_text(text.to_string())
        .map_err(|error| error.to_string())
}

fn write_clipboard_image(data_url: &str) -> Result<(), String> {
    let image = image_data_url_to_clipboard_image(data_url)?;
    let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_image(image)
        .map_err(|error| error.to_string())
}

fn image_data_url_to_clipboard_image(
    data_url: &str,
) -> Result<arboard::ImageData<'static>, String> {
    let encoded = data_url
        .strip_prefix(PNG_DATA_URL_PREFIX)
        .ok_or_else(|| "Unsupported image history format.".to_string())?;
    let png_bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|error| error.to_string())?;
    let decoded = image::load_from_memory(&png_bytes).map_err(|error| error.to_string())?;
    let rgba = decoded.to_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;

    Ok(arboard::ImageData {
        width,
        height,
        bytes: Cow::Owned(rgba.into_raw()),
    })
}

fn image_signature(width: usize, height: usize, bytes: &[u8]) -> String {
    format!("{}x{}:{:016x}", width, height, stable_hash_bytes(bytes))
}

fn stable_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;

    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    hash
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
}

#[cfg(target_os = "macos")]
fn is_accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
fn is_accessibility_trusted() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn prompt_accessibility_permission() -> bool {
    accessibility_trusted(true)
}

#[cfg(target_os = "macos")]
fn accessibility_trusted(prompt: bool) -> bool {
    use objc2::{class, msg_send, runtime::{AnyObject, Bool}};
    use objc2_foundation::ns_string;

    unsafe {
        let key = ns_string!("AXTrustedCheckOptionPrompt");
        let prompt_value = if prompt { Bool::YES } else { Bool::NO };
        let prompt_num: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithBool: prompt_value];
        let options: *const AnyObject = msg_send![
            class!(NSDictionary),
            dictionaryWithObject: prompt_num,
            forKey: key
        ];
        AXIsProcessTrustedWithOptions(options.cast())
    }
}

#[cfg(not(target_os = "macos"))]
fn prompt_accessibility_permission() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn capture_clipboard_source_app() -> Option<String> {
    capture_focus_target().map(|target| target.process_name)
}

#[cfg(not(target_os = "macos"))]
fn capture_clipboard_source_app() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn capture_focus_target() -> Option<FocusTarget> {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    let front_app = workspace.frontmostApplication()?;
    let pid = front_app.processIdentifier();

    if pid <= 0 || pid == std::process::id() as i32 {
        return None;
    }

    if let Some(bundle_id) = front_app.bundleIdentifier() {
        let bundle_str = bundle_id.to_string();
        if bundle_str == LAUNCH_AGENT_LABEL {
            return None;
        }
    }

    let process_name = front_app
        .localizedName()
        .map(|name| name.to_string())
        .unwrap_or_default()
        .trim()
        .to_string();

    if process_name == "easy-copy" || process_name == "EasyCopy" {
        return None;
    }

    Some(FocusTarget { pid, process_name })
}

#[cfg(not(target_os = "macos"))]
fn capture_focus_target() -> Option<FocusTarget> {
    None
}

#[cfg(target_os = "macos")]
fn activate_focus_target(target: &FocusTarget) -> bool {
    use objc2::{msg_send, runtime::Bool};
    use objc2_app_kit::NSRunningApplication;

    if target.pid <= 0 {
        return false;
    }

    let Some(app) =
        NSRunningApplication::runningApplicationWithProcessIdentifier(target.pid)
    else {
        return false;
    };

    // NSApplicationActivateIgnoringOtherApps = 1 << 1
    let options: usize = 1 << 1;
    let activated: Bool = unsafe { msg_send![&app, activateWithOptions: options] };
    activated.as_bool()
}

#[cfg(not(target_os = "macos"))]
fn activate_focus_target(_target: &FocusTarget) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn wait_for_target_frontmost(target: &FocusTarget, timeout_ms: u64) {
    use objc2_app_kit::NSWorkspace;

    if target.pid <= 0 {
        return;
    }

    let deadline = now_millis().saturating_add(timeout_ms);
    while now_millis() < deadline {
        let is_front = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|app| app.processIdentifier() == target.pid);
        if is_front {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "macos"))]
fn wait_for_target_frontmost(_target: &FocusTarget, _timeout_ms: u64) {}

#[cfg(target_os = "macos")]
fn paste_into_focus_target(_target: Option<&FocusTarget>) -> Result<(), String> {
    simulate_paste()
}

#[cfg(not(target_os = "macos"))]
fn paste_into_focus_target(_target: Option<&FocusTarget>) -> Result<(), String> {
    simulate_paste()
}

#[cfg(target_os = "macos")]
fn simulate_paste() -> Result<(), String> {
    use core_graphics::{
        event::{CGEvent, CGEventFlags, CGEventTapLocation},
        event_source::{CGEventSource, CGEventSourceStateID},
    };

    const VIRTUAL_KEY_COMMAND: u16 = 0x37;
    const VIRTUAL_KEY_V: u16 = 0x09;

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .or_else(|_| CGEventSource::new(CGEventSourceStateID::HIDSystemState))
        .map_err(|_| "无法创建 macOS 键盘事件源。".to_string())?;

    let cmd_down = CGEvent::new_keyboard_event(source.clone(), VIRTUAL_KEY_COMMAND, true)
        .map_err(|_| "无法创建 Command 按下事件。".to_string())?;
    cmd_down.set_flags(CGEventFlags::CGEventFlagCommand);

    let key_down = CGEvent::new_keyboard_event(source.clone(), VIRTUAL_KEY_V, true)
        .map_err(|_| "无法创建 Cmd+V 按下事件。".to_string())?;
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);

    let key_up = CGEvent::new_keyboard_event(source.clone(), VIRTUAL_KEY_V, false)
        .map_err(|_| "无法创建 Cmd+V 抬起事件。".to_string())?;
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);

    let cmd_up = CGEvent::new_keyboard_event(source, VIRTUAL_KEY_COMMAND, false)
        .map_err(|_| "无法创建 Command 抬起事件。".to_string())?;
    cmd_up.set_flags(CGEventFlags::empty());

    cmd_down.post(CGEventTapLocation::HID);
    key_down.post(CGEventTapLocation::HID);
    thread::sleep(Duration::from_millis(12));
    key_up.post(CGEventTapLocation::HID);
    cmd_up.post(CGEventTapLocation::HID);

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn simulate_paste() -> Result<(), String> {
    Err("Automatic paste is only implemented for macOS.".to_string())
}
