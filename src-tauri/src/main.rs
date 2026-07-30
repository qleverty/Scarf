#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;
mod state;

use state::new_state;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
async fn open_file_dialog(app: tauri::AppHandle) -> Option<String> {
    app.dialog().file().blocking_pick_file().map(|fp| fp.to_string())
}

#[tauri::command]
async fn save_file_dialog(app: tauri::AppHandle, default_name: String) -> Option<String> {
    app.dialog().file().set_file_name(&default_name).blocking_save_file().map(|fp| fp.to_string())
}

#[tauri::command]
async fn file_metadata(path: String) -> Result<serde_json::Value, String> {
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    let name = std::path::Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
    Ok(serde_json::json!({"name":name,"size":meta.len(),"mime":mime}))
}

// Переводит текущее (единственное) окно "main" на новый адрес.
//
// ВАЖНО: раньше здесь была навигация через eval("location.replace(...)"),
// то есть инициированная СКРИПТОМ ВНУТРИ самой веб-страницы. У Tauri v2 есть
// защитный guard, который блокирует именно такую программную навигацию окна
// на произвольный (не задекларированный) origin — это защита от того, чтобы
// скомпрометированная/вредоносная страница не увела окно приложения на левый
// адрес. Из-за этого guard'а location.replace() тихо не срабатывал: URL не
// менялся, скрипт server.html/client.html никогда не запускался — снаружи
// это выглядело как "пустое окно, будто вообще нет бэкенда".
//
// Правильный способ — переключать URL из ДОВЕРЕННОГО Rust-кода через
// WebviewWindow::navigate(), которая не проходит через этот guard, т.к.
// инициируется самим хостом приложения, а не содержимым страницы.
fn navigate_main(app: &tauri::AppHandle, target: url::Url) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("main window not found")?;
    window.navigate(target).map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_server(app: tauri::AppHandle, state: tauri::State<'_, AppState>, port: Option<u16>) -> Result<u16, String> {
    let port = port.unwrap_or(4242);
    let shared = state.shared.clone();

    std::thread::spawn(move || {
        if let Err(e) = server::run_server(shared, port) {
            eprintln!("Server error: {e}");
        }
    });

    // Ждём пока actix реально забиндит порт (макс 5 сек, проверяем TCP)
    let ready = tokio::task::spawn_blocking(move || {
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
                return true;
            }
        }
        false
    }).await.unwrap_or(false);

    if !ready {
        return Err("Server did not start in time".into());
    }

    let target = url::Url::parse(&format!("http://127.0.0.1:{}/server", port))
        .map_err(|e| e.to_string())?;
    navigate_main(&app, target)?;

    Ok(port)
}

#[tauri::command]
async fn connect_to(app: tauri::AppHandle, address: String) -> Result<(), String> {
    // Нормализуем адрес сервера в полный http:// URL
    let server_url = if address.starts_with("http") {
        address.trim_end_matches('/').to_string()
    } else if address.contains(':') {
        format!("http://{}", address)
    } else {
        format!("http://{}:4242", address)
    };

    let window = app.get_webview_window("main").ok_or("main window not found")?;
    // Берём текущий URL окна (index.html на каком бы origin/схеме он ни жил —
    // tauri://localhost, http://tauri.localhost и т.д.) и резолвим относительно
    // него client.html — так получаем корректный абсолютный URL независимо от
    // платформы, без ручного угадывания схемы.
    let current = window.url().map_err(|e| e.to_string())?;
    let mut target = current.join("client.html").map_err(|e| e.to_string())?;
    target.query_pairs_mut().append_pair("server", &server_url);

    navigate_main(&app, target)?;

    Ok(())
}

struct AppState { shared: state::SharedState }

fn main() {
    let shared = new_state();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .manage(AppState { shared })
        .invoke_handler(tauri::generate_handler![
            open_file_dialog, save_file_dialog, file_metadata, start_server, connect_to,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}