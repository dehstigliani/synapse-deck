mod claude;
mod terminal;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use claude::{SessionSummary, SessionUsage};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use terminal::{Registry, Terminal, TerminalInfo};
use tower_http::services::ServeDir;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = Arc::new(Registry::default());

    let app = Router::new()
        .route("/api/terminals", get(list_terminals).post(create_terminal))
        .route(
            "/api/terminals/:id",
            get(get_terminal).patch(update_terminal).delete(kill_terminal),
        )
        .route("/api/terminals/:id/claude-session", get(terminal_session))
        .route("/api/groups", get(list_groups))
        .route("/api/usage", get(usage_report))
        .route("/api/sessions/by-boot", get(sessions_by_boot))
        .route("/api/sessions/active", get(active_sessions))
        .route("/api/sessions", get(list_sessions))
        .route("/ws/:id", get(attach_terminal))
        .fallback_service(ServeDir::new("web"))
        .with_state(registry);

    let listener = tokio::net::TcpListener::bind(LISTEN_ADDRESS).await?;
    println!("workspace no ar em http://{LISTEN_ADDRESS}");
    println!("os terminais sobrevivem ao fechar o browser — feche a janela e volte");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn list_terminals(State(registry): State<Arc<Registry>>) -> Json<Vec<TerminalInfo>> {
    Json(registry.list())
}

async fn list_groups(State(registry): State<Arc<Registry>>) -> Json<Vec<String>> {
    Json(registry.groups())
}

/// Histórico de sessões do Claude Code num diretório — alimenta o painel de sessões.
async fn list_sessions(Query(query): Query<CwdQuery>) -> Json<Vec<SessionSummary>> {
    Json(claude::list_sessions(&query.cwd))
}

/// Consumo agregado de todos os projetos: totais, por dia e por projeto.
async fn usage_report(Query(query): Query<UsageQuery>) -> Json<claude::UsageReport> {
    Json(claude::usage_report(query.days.unwrap_or(14)))
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
    registry
        .update(&id, request.name, request.group)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn kill_terminal(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> StatusCode {
    match registry.remove(&id) {
        Some(()) => StatusCode::NO_CONTENT,
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
