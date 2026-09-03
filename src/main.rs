mod terminal;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use terminal::{latest_claude_session, Registry, Terminal, TerminalInfo};
use tower_http::services::ServeDir;

/// Porta do daemon. Só escuta em loopback: nada deste workspace vai para a rede.
const LISTEN_ADDRESS: &str = "127.0.0.1:7788";

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
    /// Diretório do terminal. Ausente = diretório onde o daemon subiu.
    cwd: Option<String>,
    /// Comando a rodar. Ausente = shell padrão.
    command: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

#[derive(Serialize)]
struct ClaudeSessionResponse {
    /// Id da sessão do Claude Code detectada para o diretório do terminal.
    session_id: Option<String>,
    /// Comando pronto para retomar essa sessão.
    resume_command: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = Arc::new(Registry::default());

    let app = Router::new()
        .route("/api/terminals", get(list_terminals).post(create_terminal))
        .route(
            "/api/terminals/:id",
            get(get_terminal).patch(rename_terminal).delete(kill_terminal),
        )
        .route("/api/terminals/:id/claude-session", get(claude_session))
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
) -> Result<Json<TerminalInfo>, (StatusCode, String)> {
    let cwd = request.cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });
    let command = request.command.unwrap_or_else(default_shell);

    registry
        .create(
            request.name,
            cwd,
            command,
            request.cols.unwrap_or(120),
            request.rows.unwrap_or(30),
        )
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn rename_terminal(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<TerminalInfo>, StatusCode> {
    registry
        .rename(&id, request.name)
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

/// Diz qual sessão do Claude Code corresponde ao diretório deste terminal.
///
/// É a ponte com o `claude-session-index`: o terminal sabe em que sessão está,
/// então dá para retomá-la depois sem caçar UUID na mão.
async fn claude_session(
    Path(id): Path<String>,
    State(registry): State<Arc<Registry>>,
) -> Result<Json<ClaudeSessionResponse>, StatusCode> {
    let found = registry.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let session_id = latest_claude_session(&found.cwd);
    let resume_command = session_id
        .as_ref()
        .map(|session| format!("claude --resume {session}"));
    Ok(Json(ClaudeSessionResponse {
        session_id,
        resume_command,
    }))
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
