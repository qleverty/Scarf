use crate::state::{FileEntry, Message, SharedState};
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use futures_util::StreamExt;
use mime_guess::from_path;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{timeout, Duration};
use uuid::Uuid;

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn is_local(req: &HttpRequest) -> bool {
    let addr = req.connection_info().peer_addr().map(|s| s.to_string()).unwrap_or_default();
    addr.starts_with("127.0.0.1") || addr.starts_with("::1") || addr.starts_with("[::1]")
}

pub async fn api_connect(state: web::Data<SharedState>) -> impl Responder {
    {
        let mut d = state.data.lock().unwrap();
        if d.connected {
            return HttpResponse::Conflict().json(serde_json::json!({"error":"busy"}));
        }
        d.connected = true;
    }
    state.push(Message { system: Some(true), text: Some("Client connected".into()), ts: now(), ..Default::default() });
    HttpResponse::Ok().json(serde_json::json!({"ok":true}))
}

pub async fn api_disconnect(state: web::Data<SharedState>) -> impl Responder {
    {
        let mut d = state.data.lock().unwrap();
        if !d.connected { return HttpResponse::Ok().json(serde_json::json!({"ok":true})); }
        d.connected = false;
    }
    state.push(Message { system: Some(true), text: Some("Client disconnected".into()), ts: now(), ..Default::default() });
    HttpResponse::Ok().json(serde_json::json!({"ok":true}))
}

pub async fn api_send(state: web::Data<SharedState>, mut payload: actix_multipart::Multipart) -> impl Responder {
    let mut from = String::from("client");
    let mut text = String::new();
    let mut msg_type: Option<String> = None;
    let mut offer_id: Option<String> = None;
    let mut file_name_field: Option<String> = None;
    let mut file_size_field: Option<u64> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut orig_file_name: Option<String> = None;
    let mut orig_mime: Option<String> = None;

    while let Some(Ok(mut field)) = payload.next().await {
        let cd = field.content_disposition().cloned();
        let fname = cd.as_ref().and_then(|c| c.get_name()).unwrap_or("").to_string();
        match fname.as_str() {
            "from" => { let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} from=String::from_utf8_lossy(&b).to_string(); }
            "text" => { let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} text=String::from_utf8_lossy(&b).trim().to_string(); }
            "type" => { let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} msg_type=Some(String::from_utf8_lossy(&b).to_string()); }
            "offer_id" => { let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} offer_id=Some(String::from_utf8_lossy(&b).to_string()); }
            "file_name" => { let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} file_name_field=Some(String::from_utf8_lossy(&b).to_string()); }
            "file_size" => { let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} file_size_field=String::from_utf8_lossy(&b).trim().parse::<u64>().ok(); }
            "file" => {
                orig_file_name=cd.as_ref().and_then(|c|c.get_filename()).and_then(|n|std::path::Path::new(n).file_name()?.to_str().map(|s|s.to_string()));
                orig_mime=field.content_type().map(|m|m.to_string()).or_else(||orig_file_name.as_deref().map(|n|from_path(n).first_or_octet_stream().to_string()));
                let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} file_bytes=Some(b);
            }
            _ => { while let Some(Ok(_))=field.next().await{} }
        }
    }

    if msg_type.as_deref() == Some("file_offer") {
        let oid = offer_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        state.push(Message {
            from: Some(from), r#type: Some("file_offer".into()),
            offer_id: Some(oid),
            file_name: Some(file_name_field.unwrap_or_else(|| "file".into())),
            file_size: Some(file_size_field.unwrap_or(0)),
            ts: now(), ..Default::default()
        });
        return HttpResponse::Ok().json(serde_json::json!({"ok":true}));
    }

    if text.is_empty() && file_bytes.is_none() {
        return HttpResponse::BadRequest().json(serde_json::json!({"error":"empty"}));
    }

    let mut msg = Message {
        from: Some(from),
        text: if text.is_empty() { None } else { Some(text) },
        ts: now(), ..Default::default()
    };

    if let Some(data) = file_bytes {
        let fid = Uuid::new_v4().to_string();
        let fn_ = orig_file_name.unwrap_or_else(|| "file".into());
        let size = data.len() as u64;
        let mime = orig_mime.unwrap_or_else(|| "application/octet-stream".into());
        let tmp = std::env::temp_dir().join(format!("{}_{}", fid, fn_));
        if let Err(e) = std::fs::write(&tmp, &data) {
            return HttpResponse::InternalServerError().json(serde_json::json!({"error":e.to_string()}));
        }
        state.data.lock().unwrap().files.insert(fid.clone(), FileEntry { name: fn_.clone(), path: tmp.to_string_lossy().to_string(), size, mime });
        msg.file_id = Some(fid); msg.file_name = Some(fn_); msg.file_size = Some(size);
    }

    state.push(msg);
    HttpResponse::Ok().json(serde_json::json!({"ok":true}))
}

#[derive(Deserialize)] pub struct RegisterFileReq { pub path: String, pub name: String, pub size: u64, pub mime: Option<String> }

pub async fn api_register_file(req: HttpRequest, state: web::Data<SharedState>, body: web::Json<RegisterFileReq>) -> impl Responder {
    if !is_local(&req) { return HttpResponse::Forbidden().body("Forbidden"); }
    let fid = Uuid::new_v4().to_string();
    let mime = body.mime.clone().unwrap_or_else(|| from_path(&body.name).first_or_octet_stream().to_string());
    state.data.lock().unwrap().files.insert(fid.clone(), FileEntry { name: body.name.clone(), path: body.path.clone(), size: body.size, mime });
    HttpResponse::Ok().json(serde_json::json!({"ok":true,"file_id":fid}))
}

#[derive(Deserialize)] pub struct SendFileMsgReq { pub from: String, pub file_id: String, pub file_name: String, pub file_size: u64, pub text: Option<String> }

pub async fn api_send_file_msg(req: HttpRequest, state: web::Data<SharedState>, body: web::Json<SendFileMsgReq>) -> impl Responder {
    if !is_local(&req) { return HttpResponse::Forbidden().body("Forbidden"); }
    state.push(Message {
        from: Some(body.from.clone()), text: body.text.clone().filter(|t|!t.is_empty()),
        file_id: Some(body.file_id.clone()), file_name: Some(body.file_name.clone()), file_size: Some(body.file_size),
        ts: now(), ..Default::default()
    });
    HttpResponse::Ok().json(serde_json::json!({"ok":true}))
}

#[derive(Deserialize)] pub struct AcceptOfferReq { pub offer_id: String, pub save_path: String }

pub async fn api_accept_offer(req: HttpRequest, state: web::Data<SharedState>, body: web::Json<AcceptOfferReq>) -> impl Responder {
    if !is_local(&req) { return HttpResponse::Forbidden().body("Forbidden"); }
    state.data.lock().unwrap().pending_saves.insert(body.offer_id.clone(), body.save_path.clone());
    state.push(Message { system: Some(true), r#type: Some("file_accept".into()), offer_id: Some(body.offer_id.clone()), text: Some("file_accept".into()), ts: now(), ..Default::default() });
    HttpResponse::Ok().json(serde_json::json!({"ok":true}))
}

pub async fn api_upload(state: web::Data<SharedState>, path: web::Path<String>, mut payload: actix_multipart::Multipart) -> impl Responder {
    let offer_id = path.into_inner();
    let save_path = { state.data.lock().unwrap().pending_saves.get(&offer_id).cloned() };
    let save_path = match save_path {
        Some(p) => p,
        None => return HttpResponse::BadRequest().json(serde_json::json!({"error":"unknown offer"})),
    };
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut fname = "file".to_string();
    let mut fmime = "application/octet-stream".to_string();
    while let Some(Ok(mut field)) = payload.next().await {
        let cd = field.content_disposition().cloned();
        if cd.as_ref().and_then(|c|c.get_name()) == Some("file") {
            fname = cd.as_ref().and_then(|c|c.get_filename()).and_then(|n|std::path::Path::new(n).file_name()?.to_str().map(|s|s.to_string())).unwrap_or_else(||"file".into());
            fmime = field.content_type().map(|m|m.to_string()).unwrap_or_else(|| from_path(&fname).first_or_octet_stream().to_string());
            let mut b=Vec::new(); while let Some(Ok(c))=field.next().await{b.extend_from_slice(&c);} file_bytes=Some(b);
        } else { while let Some(Ok(_))=field.next().await{} }
    }
    let data = match file_bytes { Some(d)=>d, None=>return HttpResponse::BadRequest().json(serde_json::json!({"error":"no file"})) };

    // Режим браузера: save_path начинается с "__browser__:" — сохраняем в памяти,
    // сервер скачает через /api/file/{fid} прямо в браузере.
    if save_path.starts_with("__browser__:") {
        let fid = Uuid::new_v4().to_string();
        let size = data.len() as u64;
        let tmp = std::env::temp_dir().join(format!("{}_{}", fid, fname));
        if let Err(e) = std::fs::write(&tmp, &data) {
            let err_str = e.to_string();
            state.push(Message { system: Some(true), r#type: Some("file_error".into()), offer_id: Some(offer_id), text: Some(err_str.clone()), ts: now(), ..Default::default() });
            return HttpResponse::InternalServerError().json(serde_json::json!({"error":err_str}));
        }
        state.data.lock().unwrap().files.insert(fid.clone(), FileEntry { name: fname.clone(), path: tmp.to_string_lossy().to_string(), size, mime: fmime });
        state.data.lock().unwrap().pending_saves.remove(&offer_id);
        // Сообщаем серверному UI: файл готов, можно скачать по file_id
        state.push(Message { system: Some(true), r#type: Some("file_ready".into()), offer_id: Some(offer_id), file_id: Some(fid), file_name: Some(fname.clone()), file_size: Some(size), text: Some(format!("Файл получен: {}", fname)), ts: now(), ..Default::default() });
        return HttpResponse::Ok().json(serde_json::json!({"ok":true}));
    }

    match std::fs::write(&save_path, &data) {
        Ok(_) => {
            state.data.lock().unwrap().pending_saves.remove(&offer_id);
            state.push(Message { system: Some(true), text: Some(format!("Файл получен: {}", fname)), ts: now(), ..Default::default() });
            HttpResponse::Ok().json(serde_json::json!({"ok":true}))
        }
        Err(e) => {
            let err_str = if e.kind()==std::io::ErrorKind::PermissionDenied { "Нет прав на запись".to_string() } else { e.to_string() };
            state.push(Message { system: Some(true), r#type: Some("file_error".into()), offer_id: Some(offer_id), text: Some(err_str.clone()), ts: now(), ..Default::default() });
            HttpResponse::InternalServerError().json(serde_json::json!({"error":err_str}))
        }
    }
}

pub async fn api_poll(state: web::Data<SharedState>, query: web::Query<std::collections::HashMap<String, String>>) -> impl Responder {
    let since: i64 = query.get("since").and_then(|s|s.parse().ok()).unwrap_or(-1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);

    // Клонируем receiver ДО первой проверки сообщений.
    // Если push случится между клонированием и первым .changed() — он вернётся сразу.
    let mut rx = state.watch_rx.clone();

    loop {
        let (msgs, connected) = {
            let d = state.data.lock().unwrap();
            let msgs: Vec<Message> = d.messages.iter().filter(|m| m.id as i64 > since).cloned().collect();
            (msgs, d.connected)
        };

        if !msgs.is_empty() {
            return HttpResponse::Ok().json(serde_json::json!({"messages":msgs,"connected":connected}));
        }

        let now_i = tokio::time::Instant::now();
        if now_i >= deadline {
            let connected = state.data.lock().unwrap().connected;
            return HttpResponse::Ok().json(serde_json::json!({"messages":[],"connected":connected}));
        }

        let remaining = deadline - now_i;
        // Async ожидание — поток не блокируется
        let _ = timeout(remaining, rx.changed()).await;
    }
}

pub async fn api_status(state: web::Data<SharedState>) -> impl Responder {
    let d = state.data.lock().unwrap();
    HttpResponse::Ok().json(serde_json::json!({"connected":d.connected,"total":d.messages.len()}))
}

pub async fn api_file(state: web::Data<SharedState>, path: web::Path<String>) -> impl Responder {
    let fid = path.into_inner();
    let entry = { state.data.lock().unwrap().files.get(&fid).cloned() };
    match entry {
        None => HttpResponse::NotFound().body("Not found"),
        Some(f) => match std::fs::read(&f.path) {
            Ok(data) => HttpResponse::Ok().content_type(f.mime.clone())
                .append_header(("Content-Disposition", format!("attachment; filename=\"{}\"", f.name)))
                .body(data),
            Err(_) => HttpResponse::NotFound().body("File not found on disk"),
        },
    }
}

// / → клиентский интерфейс (для всех)
pub async fn page_root() -> impl Responder {
    HttpResponse::Ok().content_type("text/html; charset=utf-8").body(include_str!("../../ui/client.html"))
}

// /server → серверный интерфейс (только localhost)
pub async fn page_server(req: HttpRequest) -> impl Responder {
    if !is_local(&req) { return HttpResponse::Forbidden().body("Forbidden"); }
    HttpResponse::Ok().content_type("text/html; charset=utf-8").body(include_str!("../../ui/server.html"))
}

pub fn run_server(state: SharedState, port: u16) -> std::io::Result<()> {
    let data = web::Data::new(state);
    actix_web::rt::System::new().block_on(async move {
        HttpServer::new(move || {
            App::new()
                .app_data(data.clone())
                .app_data(web::JsonConfig::default().limit(64 * 1024 * 1024))
                .route("/", web::get().to(page_root))
                .route("/server", web::get().to(page_server))
                .route("/api/connect", web::post().to(api_connect))
                .route("/api/disconnect", web::post().to(api_disconnect))
                .route("/api/send", web::post().to(api_send))
                .route("/api/register_file", web::post().to(api_register_file))
                .route("/api/send_file_msg", web::post().to(api_send_file_msg))
                .route("/api/accept_offer", web::post().to(api_accept_offer))
                .route("/api/upload/{offer_id}", web::post().to(api_upload))
                .route("/api/poll", web::get().to(api_poll))
                .route("/api/status", web::get().to(api_status))
                .route("/api/file/{fid}", web::get().to(api_file))
        })
        .bind(("0.0.0.0", port))?
        .run()
        .await
    })
}