// Sem console no Windows: clicar no atalho do menu Iniciar não pode abrir uma
// janela preta de terminal. O Deck é um programa, não um serviço de linha de
// comando — quem quiser as mensagens roda pelo terminal, que aí elas aparecem.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod claude;
mod terminal;
mod update;
mod workspace;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use rust_embed::RustEmbed;
use claude::{SessionSummary, SessionUsage};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use terminal::{Registry, Terminal, TerminalInfo};

/// A interface inteira vai dentro do binário: um release é um arquivo só, e o
/// programa roda de qualquer diretório em vez de exigir a pasta `web/` ao lado.
#[derive(RustEmbed)]
#[folder = "web/"]
struct WebAssets;

/// Porta do daemon. Só escuta em loopback: nada deste workspace vai para a rede.
const LISTEN_ADDRESS: &str = "127.0.0.1:7788";

/// Nome da pasta usada quando o terminal não declara nenhuma.
const DEFAULT_GROUP: &str = "Geral";

/// O que o browser manda pelo WebSocket.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    /// Teclas digitadas no xterm.js.
    Input { data: String },
    /// Novo tamanho da janela, para o PTY reflowar.
    Resize { cols: u16, rows: u16 },
}

#[derive(Deserialize)]
struct CreateRequest {
    name: String,
    /// A "pasta" da sidebar.
    group: Option<String>,
    cwd: Option<String>,
    command: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    /// Sessão a retomar: vira `claude --resume <id>`.
    resume: Option<String>,
    /// Marcar a pasta como confiável antes de subir o Claude. Padrão: sim.
    trust: Option<bool>,
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

#[derive(Deserialize)]
struct UpdateRequest {
    name: Option<String>,
    group: Option<String>,
}

#[derive(Deserialize)]
struct CwdQuery {
    cwd: String,
}

#[derive(Deserialize)]
struct UsageQuery {
    days: Option<usize>,
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WithinQuery {
    /// Janela em segundos para considerar a sessao ainda ativa.
    within: Option<u64>,
}

#[derive(Serialize)]
struct CreateResponse {
    #[serde(flatten)]
    terminal: TerminalInfo,
    /// Se a pasta precisou ser marcada como confiável agora.
    trusted_now: bool,
}

#[derive(Serialize)]
struct TerminalSession {
    session: Option<SessionSummary>,
    /// Atalho para a sidebar não ter que cavar dentro de `session`.
    usage: Option<SessionUsage>,
}

/// Abre a interface numa janela própria.
///
/// Sem Tauri ainda, o mais próximo de janela nativa é o modo aplicativo do
/// navegador: sem barra de endereço, sem abas, com ícone e entrada próprios na
/// barra de tarefas. Se nenhum navegador conhecido existir, cai no padrão do
/// sistema, que abre numa aba comum.
fn open_window(address: &str) {
    let url = format!("http://{address}");

    #[cfg(windows)]
    {
        use std::process::Command;

        const APP_BROWSERS: [&str; 3] = [
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ];

        for browser in APP_BROWSERS {
            if std::path::Path::new(browser).exists() {
                if Command::new(browser)
                    .arg(format!("--app={url}"))
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        }
        // Último recurso: quem abre é o sistema, numa aba comum.
        let _ = Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
}

/// Já existe um Deck rodando nesta máquina?
///
/// Clicar duas vezes no atalho não pode dar erro de porta ocupada: se o daemon
/// já está de pé, a segunda execução só traz a janela de volta e sai.
fn already_running(address: &str) -> bool {
    std::net::TcpStream::connect_timeout(
        &address.parse().expect("endereço de escuta inválido"),
        std::time::Duration::from_millis(300),
    )
    .is_ok()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if already_running(LISTEN_ADDRESS) {
        open_window(LISTEN_ADDRESS);
        return Ok(());
    }

    let registry = Arc::new(Registry::default());

    // Recria o workspace da execucao anterior. Os PTYs morrem com o processo,
    // mas a forma do workspace nao precisa morrer junto.
    let restored = workspace::restore(&registry);
    if restored > 0 {
        println!("{restored} terminais restaurados do workspace anterior");
    }

    let app = Router::new()
        .route("/api/terminals", get(list_terminals).post(create_terminal))
        .route(
            "/api/terminals/:id",
            get(get_terminal).patch(update_terminal).delete(kill_terminal),
        )
        .route("/api/terminals/:id/claude-session", get(terminal_session))
        .route("/api/groups", get(list_groups))
        .route("/api/groups/:name", axum::routing::patch(rename_group))
        .route("/api/usage", get(usage_report))
        .route("/api/usage/pulse", get(usage_pulse))
        .route("/api/update", get(check_update).post(apply_update))
        .route("/api/sessions/by-boot", get(sessions_by_boot))
        .route("/api/sessions/active", get(active_sessions))
        .route("/api/sessions", get(list_sessions))
        .route("/ws/:id", get(attach_terminal))
        .fallback(serve_asset)
        .with_state(registry);

    // Limpa o binário antigo deixado por uma atualização anterior.
    update::clean_previous();

    let listener = tokio::net::TcpListener::bind(LISTEN_ADDRESS).await?;
    println!("Synapse Deck no ar em http://{LISTEN_ADDRESS}");
    println!("os terminais sobrevivem ao fechar a janela — feche e volte");

    // A janela sobe depois que a porta já aceita conexão, senão ela abre antes
    // do servidor responder e mostra erro no primeiro carregamento.
    open_window(LISTEN_ADDRESS);

    axum::serve(listener, app).await?;
    Ok(())
}

/// Serve a interface embutida no binário.
async fn serve_asset(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match WebAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "não encontrado").into_response(),
    }
}

async fn list_terminals(State(registry): State<Arc<Registry>>) -> Json<Vec<TerminalInfo>> {
    Json(registry.list())
}

async fn list_groups(State(registry): State<Arc<Registry>>) -> Json<Vec<String>> {
    Json(registry.groups())
}

/// Renomeia uma pasta inteira.
async fn rename_group(
    Path(name): Path<String>,
    State(registry): State<Arc<Registry>>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let novo = request.name.trim();
    if novo.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let moved = registry.rename_group(&name, novo);
    persist(&registry);
    Ok(Json(serde_json::json!({ "moved": moved })))
}

/// Grava o workspace depois de qualquer mudanca de forma.
///
/// Falhar aqui nao pode derrubar a operacao que o usuario pediu: no pior caso
/// ele perde a restauracao, nao o terminal que acabou de criar.
fn persist(registry: &Arc<Registry>) {
    if let Err(error) = workspace::save(registry) {
        eprintln!("workspace nao foi salvo: {error}");
    }
}

/// Histórico de sessões do Claude Code num diretório — alimenta o painel de sessões.
async fn list_sessions(Query(query): Query<CwdQuery>) -> Json<Vec<SessionSummary>> {
    Json(claude::list_sessions(&query.cwd))
}

/// Consumo agregado de todos os projetos: totais, por dia e por projeto.
async fn usage_report(Query(query): Query<UsageQuery>) -> Json<claude::UsageReport> {
    Json(claude::usage_report(query.days.unwrap_or(14)))
}

/// Ha versao nova publicada?
///
/// A consulta sai numa thread de bloqueio: `ureq` e sincrono e travaria o
/// executor async se rodasse direto no handler.
async fn check_update() -> Json<update::UpdateStatus> {
    Json(
        tokio::task::spawn_blocking(update::check)
            .await
            .unwrap_or_default(),
    )
}

/// Baixa a versao nova e troca o executavel.
///
/// Nao reinicia o programa: reiniciar mataria todos os terminais abertos. A
/// versao nova vale a partir da proxima inicializacao.
async fn apply_update() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let result = tokio::task::spawn_blocking(update::apply)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    match result {
        Ok(path) => Ok(Json(serde_json::json!({
            "installed_at": path.to_string_lossy(),
            "message": "Atualizado. A versão nova vale na próxima vez que o Deck abrir —                         os terminais em uso continuam intactos até lá.",
        }))),
        Err(error) => Err((StatusCode::BAD_GATEWAY, error.to_string())),
    }
}

/// Quanto foi consumido na ultima hora, na janela de 5h e na semana.
async fn usage_pulse() -> Json<claude::UsagePulse> {
    Json(claude::usage_pulse())
}

/// Sessoes agrupadas pelo boot do Windows em que estavam vivas.
async fn sessions_by_boot(Query(query): Query<LimitQuery>) -> Json<Vec<claude::BootGroup>> {
    Json(claude::sessions_by_boot(query.limit.unwrap_or(8)))
}

/// Sessoes escritas ha pouco: provavelmente abertas fora deste workspace.
async fn active_sessions(Query(query): Query<WithinQuery>) -> Json<Vec<SessionSummary>> {
    Json(claude::active_sessions(query.within.unwrap_or(900)))
}

async fn get_terminal(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> Result<Json<TerminalInfo>, StatusCode> {
    registry
        .get(&id)
        .map(|found| Json(found.info()))
        .ok_or(StatusCode::NOT_FOUND)
}

async fn create_terminal(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<CreateRequest>,
) -> Result<Json<CreateResponse>, (StatusCode, String)> {
    let cwd = request.cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    let command = match (request.command, request.resume) {
        // Retomar uma sessão do histórico é só um `claude` com argumento.
        (_, Some(session)) => format!("claude --resume {session}"),
        (Some(command), None) => command,
        (None, None) => default_shell(),
    };

    // Marca a pasta como confiável antes de subir, senão o Claude abre no
    // "do you trust this folder" e o terminal nasce esperando resposta.
    let wants_trust = request.trust.unwrap_or(true);
    let trusted_now = if wants_trust && command.contains("claude") {
        claude::trust_directory(&cwd).unwrap_or(false)
    } else {
        false
    };

    let terminal = registry
        .create(
            request.name,
            request
                .group
                .filter(|group| !group.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_GROUP.to_string()),
            cwd,
            command,
            request.cols.unwrap_or(120),
            request.rows.unwrap_or(30),
        )
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    persist(&registry);

    Ok(Json(CreateResponse {
        terminal,
        trusted_now,
    }))
}

async fn update_terminal(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<TerminalInfo>, StatusCode> {
    let updated = registry
        .update(&id, request.name, request.group)
        .ok_or(StatusCode::NOT_FOUND)?;
    persist(&registry);
    Ok(Json(updated))
}

async fn kill_terminal(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> StatusCode {
    match registry.remove(&id) {
        Some(()) => {
            persist(&registry);
            StatusCode::NO_CONTENT
        }
        None => StatusCode::NOT_FOUND,
    }
}

/// Em que sessão do Claude Code este terminal está, e quanto ela já consumiu.
async fn terminal_session(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> Result<Json<TerminalSession>, StatusCode> {
    let found = registry.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let session = claude::latest_session(&found.cwd);
    let usage = session.as_ref().map(|summary| summary.usage.clone());
    Ok(Json(TerminalSession { session, usage }))
}

async fn attach_terminal(
    upgrade: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> impl IntoResponse {
    match registry.get(&id) {
        Some(found) => upgrade
            .on_upgrade(move |socket| bridge(socket, found))
            .into_response(),
        None => (StatusCode::NOT_FOUND, "terminal não encontrado").into_response(),
    }
}

/// Liga um browser a um PTY já rodando: replay do scrollback, depois ao vivo.
async fn bridge(socket: WebSocket, found: Arc<Terminal>) {
    use futures_util::{SinkExt, StreamExt};

    let (mut sink, mut stream) = socket.split();

    // Assinar ANTES de tirar o snapshot. Na ordem inversa, os bytes que chegam
    // entre o snapshot e a assinatura são perdidos e a tela fica furada.
    let mut receiver = found.subscribe();
    let backlog = found.snapshot();
    if !backlog.is_empty() && sink.send(Message::Binary(backlog)).await.is_err() {
        return;
    }

    let to_pty_terminal = Arc::clone(&found);
    let mut to_pty = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Text(text) => {
                    if let Ok(parsed) = serde_json::from_str::<ClientMessage>(&text) {
                        match parsed {
                            ClientMessage::Input { data } => {
                                let _ = to_pty_terminal.write_input(data.as_bytes());
                            }
                            ClientMessage::Resize { cols, rows } => {
                                let _ = to_pty_terminal.resize(cols, rows);
                            }
                        }
                    }
                }
                Message::Binary(bytes) => {
                    let _ = to_pty_terminal.write_input(&bytes);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let mut to_client = tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(chunk) => {
                    if sink.send(Message::Binary(chunk)).await.is_err() {
                        break;
                    }
                }
                // Cliente lento: perdeu blocos, mas a sessão continua. Segue.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    // Fechar o browser encerra só a ponte. O PTY e o processo seguem vivos no
    // registry — é isso que faz a sessão de Claude sobreviver à janela.
    tokio::select! {
        _ = &mut to_pty => to_client.abort(),
        _ = &mut to_client => to_pty.abort(),
    }
}

/// Shell padrão do sistema, usado quando o terminal não é de agente.
fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}
