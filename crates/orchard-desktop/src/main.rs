#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_os = "macos"))]
compile_error!("orchard-desktop is a macOS-only menu bar application");

use orchard_update::UpdateStatus;
use orchard_workspace_host::{HostError, ServerHandle, WorkspaceHost};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::json;
use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

const TRAY_ID: &str = "orchard-tray";
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LaunchOptions {
    data_dir: Option<PathBuf>,
    port: Option<u16>,
    br_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceMenuEntry {
    id: String,
    name: String,
    root: PathBuf,
}

struct DesktopState {
    host: Arc<WorkspaceHost>,
    shutdown: Mutex<ShutdownState>,
    base_url: String,
    update_check_in_flight: AtomicBool,
}

struct ShutdownState {
    server: Option<ServerHandle>,
    phase: ShutdownPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShutdownPhase {
    Running,
    Stopping,
    Stopped,
}

impl ShutdownState {
    fn begin(&mut self) -> Option<ServerHandle> {
        if self.phase != ShutdownPhase::Running {
            return None;
        }
        self.phase = ShutdownPhase::Stopping;
        self.server.take()
    }
}

fn main() {
    let options = match LaunchOptions::parse(env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("Orchard: {error}");
            std::process::exit(2);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let data_dir = options
                .data_dir
                .clone()
                .or_else(|| env::var_os("ORCHARD_DATA_DIR").map(PathBuf::from))
                .unwrap_or_else(|| app.path().data_dir().expect("macOS data directory").join("Orchard"));
            let port = if let Some(port) = options.port {
                Some(port)
            } else {
                match env_port() {
                    Ok(port) => port,
                    Err(error) => {
                        show_startup_error(app.handle(), &error);
                        return Ok(());
                    }
                }
            };
            let br_path = options
                .br_path
                .clone()
                .or_else(|| env::var_os("ORCHARD_BR_PATH").map(PathBuf::from))
                .unwrap_or_else(|| bundled_br_path(app.handle()));

            let host = match WorkspaceHost::open_with_port(data_dir, br_path, port) {
                Ok(host) => Arc::new(host),
                Err(HostError::AlreadyRunning) => {
                    app.dialog()
                        .message("Another Orchard service already owns this data directory. The existing service was left running and no data was changed.")
                        .title("Orchard is already running")
                        .kind(MessageDialogKind::Warning)
                        .blocking_show();
                    app.handle().exit(0);
                    return Ok(());
                }
                Err(error) => {
                    show_startup_error(app.handle(), &error.to_string());
                    return Ok(());
                }
            };
            let server = match tauri::async_runtime::block_on(
                host.clone().start_server_with_ui(orchard_server::ui_router()),
            ) {
                Ok(server) => server,
                Err(error) => {
                    show_startup_error(app.handle(), &error.to_string());
                    return Ok(());
                }
            };
            let base_url = format!("http://{}", server.endpoint());
            let state = Arc::new(DesktopState {
                host,
                shutdown: Mutex::new(ShutdownState {
                    server: Some(server),
                    phase: ShutdownPhase::Running,
                }),
                base_url,
                update_check_in_flight: AtomicBool::new(false),
            });
            app.manage(state.clone());
            install_tray(app.handle(), &state)?;
            spawn_menu_refresh(app.handle().clone(), state);
            Ok(())
        })
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        .build(tauri::generate_context!())
        .expect("failed to build Orchard desktop shell")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if let Some(state) = app.try_state::<Arc<DesktopState>>() {
                    let phase = state.shutdown.lock().unwrap().phase;
                    if phase != ShutdownPhase::Stopped {
                        api.prevent_exit();
                        if phase == ShutdownPhase::Running {
                            graceful_quit(app.clone(), &state);
                        }
                    }
                }
            }
        });
}

impl LaunchOptions {
    fn parse<I>(args: I) -> Result<Self, String>
    where
        I: IntoIterator,
        I::Item: Into<std::ffi::OsString>,
    {
        let mut args = args.into_iter().map(Into::into);
        let mut options = Self::default();
        while let Some(argument) = args.next() {
            let text = argument.to_string_lossy();
            match text.as_ref() {
                "--data-dir" => {
                    options.data_dir = Some(PathBuf::from(next_value("--data-dir", &mut args)?))
                }
                "--br-path" => {
                    options.br_path = Some(PathBuf::from(next_value("--br-path", &mut args)?))
                }
                "--port" => {
                    let raw = next_value("--port", &mut args)?;
                    options.port = Some(parse_port(&raw.to_string_lossy())?);
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: Orchard [--data-dir PATH] [--port PORT] [--br-path PATH]"
                            .to_owned(),
                    )
                }
                _ => return Err(format!("unknown argument {text:?}")),
            }
        }
        Ok(options)
    }
}

fn next_value(
    name: &str,
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<std::ffi::OsString, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("invalid port {value:?}; expected 1-65535"))
}

fn env_port() -> Result<Option<u16>, String> {
    env::var("ORCHARD_PORT")
        .ok()
        .map(|value| parse_port(&value).map_err(|error| format!("ORCHARD_PORT: {error}")))
        .transpose()
}

fn bundled_br_path<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    app.path()
        .resource_dir()
        .expect("Orchard resource directory")
        .join("bin/br")
}

fn workspace_entries(host: &WorkspaceHost) -> Result<Vec<WorkspaceMenuEntry>, String> {
    let result = host.call("workspace_list", json!({}))?;
    Ok(result["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|workspace| !workspace["archived"].as_bool().unwrap_or(false))
        .filter_map(|workspace| {
            Some(WorkspaceMenuEntry {
                id: workspace["id"].as_str()?.to_owned(),
                name: workspace["name"].as_str()?.to_owned(),
                root: PathBuf::from(workspace["root"].as_str()?),
            })
        })
        .collect())
}

fn workspace_path(id: &str) -> String {
    format!("/w/{}", utf8_percent_encode(id, NON_ALPHANUMERIC))
}

fn settings_path(id: &str) -> String {
    format!("{}/settings", workspace_path(id))
}

fn install_tray<R: Runtime>(app: &AppHandle<R>, state: &Arc<DesktopState>) -> tauri::Result<()> {
    let menu = build_menu(app, state)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon())
        .icon_as_template(true)
        .tooltip("Orchard")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;
    Ok(())
}

fn build_menu<R: Runtime>(app: &AppHandle<R>, state: &DesktopState) -> tauri::Result<Menu<R>> {
    let entries = workspace_entries(&state.host).unwrap_or_default();
    let open = MenuItem::with_id(app, "open", "Open Orchard", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let workspaces = Submenu::new(app, "Workspaces", true)?;
    let settings = Submenu::new(app, "Settings", !entries.is_empty())?;
    let copy = Submenu::new(app, "Copy Workspace Info", !entries.is_empty())?;
    if entries.is_empty() {
        let create = MenuItem::with_id(
            app,
            "create-workspace",
            "Create a Workspace…",
            true,
            None::<&str>,
        )?;
        workspaces.append(&create)?;
    } else {
        for entry in &entries {
            let open_id = format!("workspace:{}", entry.id);
            let settings_id = format!("settings:{}", entry.id);
            workspaces.append(&MenuItem::with_id(
                app,
                &open_id,
                &entry.name,
                true,
                None::<&str>,
            )?)?;
            settings.append(&MenuItem::with_id(
                app,
                &settings_id,
                &entry.name,
                true,
                None::<&str>,
            )?)?;
            let tools = Submenu::new(app, &entry.name, true)?;
            tools.append(&MenuItem::with_id(
                app,
                format!("copy-prompt:{}", entry.id),
                "Copy Joining Prompt",
                true,
                None::<&str>,
            )?)?;
            tools.append(&MenuItem::with_id(
                app,
                format!("copy-path:{}", entry.id),
                "Copy Workspace Path",
                true,
                None::<&str>,
            )?)?;
            copy.append(&tools)?;
        }
    }
    let updates = MenuItem::with_id(
        app,
        "check-updates",
        "Check for Updates…",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit Orchard", true, None::<&str>)?;
    Menu::with_items(
        app,
        &[
            &open,
            &separator,
            &workspaces,
            &settings,
            &copy,
            &updates,
            &quit,
        ],
    )
}

fn spawn_menu_refresh<R: Runtime>(app: AppHandle<R>, state: Arc<DesktopState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(REFRESH_INTERVAL).await;
            let app_for_main = app.clone();
            let state_for_main = state.clone();
            if app
                .run_on_main_thread(move || {
                    if let (Some(tray), Ok(menu)) = (
                        app_for_main.tray_by_id(TRAY_ID),
                        build_menu(&app_for_main, &state_for_main),
                    ) {
                        let _ = tray.set_menu(Some(menu));
                    }
                })
                .is_err()
            {
                break;
            }
        }
    });
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let state = app.state::<Arc<DesktopState>>();
    match id {
        "open" | "create-workspace" => open_local(app, &state.base_url, "/"),
        "check-updates" => {
            check_for_updates(app.clone(), state.inner().clone());
        }
        "quit" => graceful_quit(app.clone(), &state),
        _ => {
            if let Some(workspace_id) = id.strip_prefix("workspace:") {
                open_local(app, &state.base_url, &workspace_path(workspace_id));
            } else if let Some(workspace_id) = id.strip_prefix("settings:") {
                open_local(app, &state.base_url, &settings_path(workspace_id));
            } else if let Some(workspace_id) = id.strip_prefix("copy-prompt:") {
                copy_joining_prompt(app, &state.host, workspace_id);
            } else if let Some(workspace_id) = id.strip_prefix("copy-path:") {
                if let Some(entry) = workspace_entries(&state.host)
                    .ok()
                    .and_then(|entries| entries.into_iter().find(|entry| entry.id == workspace_id))
                {
                    copy_text(app, entry.root.to_string_lossy().into_owned());
                }
            }
        }
    }
}

fn graceful_quit<R: Runtime>(app: AppHandle<R>, state: &DesktopState) {
    let server = state.shutdown.lock().unwrap().begin();
    let Some(server) = server else { return };
    let state = app.state::<Arc<DesktopState>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let _ = server.shutdown().await;
        state.shutdown.lock().unwrap().phase = ShutdownPhase::Stopped;
        app.exit(0);
    });
}

fn check_for_updates<R: Runtime>(app: AppHandle<R>, state: Arc<DesktopState>) {
    if state
        .update_check_in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let result = orchard_update::check_for_updates(env!("CARGO_PKG_VERSION")).await;
        let app_for_main = app.clone();
        let state_for_main = state.clone();
        if app
            .run_on_main_thread(move || {
                state_for_main
                    .update_check_in_flight
                    .store(false, Ordering::Release);
                if state_for_main.shutdown.lock().unwrap().phase != ShutdownPhase::Running {
                    return;
                }
                match result {
                    Ok(update) if update.status == UpdateStatus::UpdateAvailable => {
                        let open = app_for_main
                            .dialog()
                            .message(format!(
                                "Orchard {} is available. Open the release page?",
                                update.latest_version
                            ))
                            .title("Orchard Update Available")
                            .kind(MessageDialogKind::Info)
                            .buttons(MessageDialogButtons::OkCancelCustom(
                                "Open Release".to_owned(),
                                "Later".to_owned(),
                            ))
                            .blocking_show();
                        if open {
                            if let Err(error) = app_for_main
                                .opener()
                                .open_url(update.release_url, None::<&str>)
                            {
                                show_action_error(
                                    &app_for_main,
                                    &format!("Could not open the release page: {error}"),
                                );
                            }
                        }
                    }
                    Ok(update) => {
                        app_for_main
                            .dialog()
                            .message(format!("Orchard {} is up to date.", update.latest_version))
                            .title("Orchard Updates")
                            .blocking_show();
                    }
                    Err(error) => show_action_error(&app_for_main, &error.to_string()),
                }
            })
            .is_err()
        {
            state.update_check_in_flight.store(false, Ordering::Release);
        }
    });
}

fn copy_joining_prompt<R: Runtime>(app: &AppHandle<R>, host: &WorkspaceHost, workspace_id: &str) {
    match host.call("workspace_intro", json!({"workspace_id": workspace_id})) {
        Ok(value) => match value["joining_prompt"].as_str() {
            Some(prompt) => copy_text(app, prompt.to_owned()),
            None => show_action_error(app, "This workspace has no joining prompt."),
        },
        Err(error) => show_action_error(app, &error),
    }
}

fn copy_text<R: Runtime>(app: &AppHandle<R>, text: String) {
    match app.clipboard().write_text(text) {
        Ok(()) => {}
        Err(error) => show_action_error(app, &format!("Could not copy: {error}")),
    }
}

fn show_action_error<R: Runtime>(app: &AppHandle<R>, message: &str) {
    app.dialog()
        .message(message)
        .title("Orchard")
        .kind(MessageDialogKind::Error)
        .blocking_show();
}

fn show_startup_error<R: Runtime>(app: &AppHandle<R>, message: &str) {
    app.dialog()
        .message(format!(
            "Orchard could not start its local service: {message}"
        ))
        .title("Orchard could not start")
        .kind(MessageDialogKind::Error)
        .blocking_show();
    app.exit(1);
}

fn open_local<R: Runtime>(app: &AppHandle<R>, base_url: &str, path: &str) {
    let url = format!("{base_url}{path}");
    if let Err(error) = app.opener().open_url(url, None::<&str>) {
        app.dialog()
            .message(format!("Could not open Orchard in the browser: {error}"))
            .title("Orchard")
            .kind(MessageDialogKind::Error)
            .blocking_show();
    }
}

fn tray_icon() -> Image<'static> {
    const SIZE: u32 = 18;
    let mut rgba = vec![0_u8; (SIZE * SIZE * 4) as usize];
    for y in 1..SIZE - 1 {
        for x in 1..SIZE - 1 {
            let dx = x as i32 - 8;
            let dy = y as i32 - 9;
            if dx * dx + dy * dy <= 49 || (x >= 8 && x <= 10 && y <= 5) {
                let offset = ((y * SIZE + x) * 4) as usize;
                rgba[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_isolated_smoke_options() {
        let options = LaunchOptions::parse([
            "--data-dir",
            "/tmp/orchard-smoke",
            "--port",
            "43121",
            "--br-path",
            "/tmp/br",
        ])
        .unwrap();
        assert_eq!(options.data_dir, Some(PathBuf::from("/tmp/orchard-smoke")));
        assert_eq!(options.port, Some(43121));
        assert_eq!(options.br_path, Some(PathBuf::from("/tmp/br")));
    }

    #[test]
    fn rejects_zero_and_unknown_arguments() {
        assert!(LaunchOptions::parse(["--port", "0"]).is_err());
        assert!(LaunchOptions::parse(["--surprise"]).is_err());
    }

    #[test]
    fn malformed_port_env_uses_the_error_path() {
        assert!(parse_port("not-a-port").is_err());
        assert!(parse_port("0").is_err());
    }

    #[test]
    fn routes_encode_workspace_ids_and_never_include_credentials() {
        assert_eq!(workspace_path("a/b c"), "/w/a%2Fb%20c");
        assert_eq!(settings_path("a/b c"), "/w/a%2Fb%20c/settings");
    }

    #[test]
    fn workspace_menu_reads_live_host_records() {
        let temp = tempfile::tempdir().unwrap();
        let br = temp.path().join("br");
        std::fs::write(&br, "not executed by this test").unwrap();
        let host = WorkspaceHost::open(temp.path().join("data"), br).unwrap();
        host.call("workspace_create", json!({"name":"Live menu"}))
            .unwrap();
        let entries = workspace_entries(&host).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Live menu");
    }

    #[test]
    fn bundled_br_relative_path_is_stable() {
        assert_eq!(PathBuf::from("bin").join("br"), PathBuf::from("bin/br"));
    }

    #[test]
    fn repeated_shutdown_requests_do_not_take_the_server_twice() {
        let mut state = ShutdownState {
            server: None,
            phase: ShutdownPhase::Running,
        };
        assert!(state.begin().is_none());
        assert_eq!(state.phase, ShutdownPhase::Stopping);
        assert!(state.begin().is_none());
        assert_eq!(state.phase, ShutdownPhase::Stopping);
    }
}
