#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod server;
mod state;

use state::new_state;
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

#[tauri::command]
async fn start_server(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<u16, String> {
    let port = 4242u16;
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

    tauri::WebviewWindowBuilder::new(
        &app, "server",
        tauri::WebviewUrl::External(format!("http://127.0.0.1:{}/server", port).parse().unwrap()),
    )
    .title("Scarf — Server")
    .inner_size(800.0, 600.0)
    .build()
    .map_err(|e| e.to_string())?;

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

    // Открываем локальный client.html и передаём адрес сервера через query-параметр.
    // WebviewUrl::App принимает PathBuf — query string задаём через std::path::PathBuf::from.
    tauri::WebviewWindowBuilder::new(
        &app, "client",
        tauri::WebviewUrl::App(std::path::PathBuf::from(format!("client.html?server={}", server_url))),
    )
    .title("Scarf — Client")
    .inner_size(800.0, 600.0)
    .build()
    .map_err(|e| e.to_string())?;

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