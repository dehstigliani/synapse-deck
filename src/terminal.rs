use anyhow::{anyhow, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Teto do scrollback guardado por terminal.
///
/// Fica em memória de propósito. Gravar cada byte do PTY em disco é o que produz
/// escrita contínua — foi exatamente a reclamação de "queima de SSD" que auditamos
/// no Alethe (issue #22: ~27 MB/s constantes, ~2,3 TB/dia). Aqui o buffer tem teto
/// fixo e nada toca o disco.
const SCROLLBACK_LIMIT: usize = 256 * 1024;

/// Quantos blocos o canal de distribuição segura antes de descartar os mais antigos.
const BROADCAST_CAPACITY: usize = 1024;

/// Variáveis que marcam "sou uma sessão-filha de outro Claude Code".
///
/// Se o daemon foi iniciado de dentro de um Claude Code, ele herda tudo isso e
/// repassa aos filhos — e o `CLAUDE_CODE_CHILD_SESSION` faz o Claude **não
/// gravar o transcript**, que é justamente a matéria-prima do histórico de
/// sessões e do medidor de contexto. Um terminal do workspace é sessão de topo.
const INHERITED_SESSION_VARS: [&str; 11] = [
    // Porta de desenvolvimento nao deve ser herdada por um Deck aberto de dentro.
    "SYNAPSE_DECK_PORT",
    // O token do atualizador nao tem nada que fazer dentro de um terminal.
    "SYNAPSE_DECK_GITHUB_TOKEN",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDECODE",
];

/// Tamanho de leitura do PTY. Blocos maiores reduzem troca de contexto.
const READ_CHUNK: usize = 8 * 1024;

/// O que a sidebar precisa saber sobre um terminal.
#[derive(Clone, Serialize)]
pub struct TerminalInfo {
    pub id: String,
    pub name: String,
    pub group: String,
    pub cwd: String,
    pub command: String,
    pub alive: bool,
}

/// Um terminal vivo: o PTY, o processo filho e o scrollback que permite reatar.
pub struct Terminal {
    pub id: String,
    name: Mutex<String>,
    /// A "pasta" da sidebar: agrupa terminais para organização.
    group: Mutex<String>,
    pub cwd: String,
    pub command: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    scrollback: Mutex<Vec<u8>>,
    sender: broadcast::Sender<Vec<u8>>,
}

impl Terminal {
    /// Abre um PTY, sobe o comando dentro dele e começa a bombear a saída.
    pub fn spawn(
        name: String,
        group: String,
        cwd: String,
        command: String,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Self>> {
        let pty_system = NativePtySystem::default();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut builder = build_command(&command)?;
        builder.cwd(&cwd);

        let child = pair.slave.spawn_command(builder)?;

        // O slave tem que ser descartado depois do spawn: enquanto ele existir, o
        // master nunca vê EOF e o terminal parece vivo mesmo com o processo morto.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);

        let terminal = Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: Mutex::new(name),
            group: Mutex::new(group),
            cwd,
            command,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            scrollback: Mutex::new(Vec::new()),
            sender,
        });

        // Uma thread de SO por terminal: leitura de PTY é I/O bloqueante, não async.
        // É o ponto que segura o processo vivo mesmo sem nenhum browser conectado.
        let pump = Arc::clone(&terminal);
        std::thread::spawn(move || pump.pump_output(reader));

        Ok(terminal)
    }

    /// Lê o PTY até o fim da vida do processo, alimentando scrollback e assinantes.
    fn pump_output(&self, mut reader: Box<dyn Read + Send>) {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let chunk = buf[..read].to_vec();

                    {
                        let mut backlog = self.scrollback.lock().unwrap();
                        backlog.extend_from_slice(&chunk);
                        // Anel simples: corta o excedente pela frente.
                        if backlog.len() > SCROLLBACK_LIMIT {
                            let excess = backlog.len() - SCROLLBACK_LIMIT;
                            backlog.drain(..excess);
                        }
                    }

                    // Sem ninguém conectado o envio falha, e está tudo bem: quem
                    // reatar depois recebe o scrollback acima.
                    let _ = self.sender.send(chunk);
                }
            }
        }
    }

    /// Escreve o que o usuário digitou no browser dentro do PTY.
    pub fn write_input(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    /// Repassa o tamanho da janela do xterm.js para o PTY.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.sender.subscribe()
    }

    /// Cópia do scrollback, enviada a quem acabou de conectar.
    pub fn snapshot(&self) -> Vec<u8> {
        self.scrollback.lock().unwrap().clone()
    }

    pub fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }

    pub fn rename(&self, name: String) {
        *self.name.lock().unwrap() = name;
    }

    pub fn group(&self) -> String {
        self.group.lock().unwrap().clone()
    }

    pub fn set_group(&self, group: String) {
        *self.group.lock().unwrap() = group;
    }

    /// `try_wait` devolvendo `None` significa que o processo ainda está rodando.
    pub fn is_alive(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    pub fn kill(&self) -> Result<()> {
        self.child.lock().unwrap().kill()?;
        Ok(())
    }

    pub fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id.clone(),
            name: self.name(),
            group: self.group(),
            cwd: self.cwd.clone(),
            command: self.command.clone(),
            alive: self.is_alive(),
        }
    }
}

/// Monta o `CommandBuilder` a partir da linha de comando.
///
/// No Windows o `CreateProcessW` só executa binário de verdade. Os atalhos que o
/// npm instala (`claude`, `claude.cmd`) e qualquer `.bat`/`.cmd` são script, e
/// falham com "não é um aplicativo Win32 válido" (os error 193). Para esses, quem
/// executa de fato é o interpretador de comandos.
fn build_command(command: &str) -> Result<CommandBuilder> {
    let mut parts = command.split_whitespace();
    let program = parts.next().ok_or_else(|| anyhow!("comando vazio"))?;

    let is_native_binary = program.to_ascii_lowercase().ends_with(".exe");
    if cfg!(windows) && !is_native_binary {
        let interpreter = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        let mut builder = CommandBuilder::new(interpreter);
        builder.arg("/C");
        // A linha inteira vai como um argumento só — é a forma que o `/C` espera.
        builder.arg(command);
        clear_inherited_session(&mut builder);
        return Ok(builder);
    }

    let mut builder = CommandBuilder::new(program);
    for arg in parts {
        builder.arg(arg);
    }
    clear_inherited_session(&mut builder);
    Ok(builder)
}

/// Tira do filho as marcas de sessão herdadas do processo que subiu o daemon.
fn clear_inherited_session(builder: &mut CommandBuilder) {
    for variable in INHERITED_SESSION_VARS {
        builder.env_remove(variable);
    }
}

/// Os terminais do workspace, em ordem de criação — a ordem da sidebar.
#[derive(Default)]
pub struct Registry {
    terminals: Mutex<Vec<Arc<Terminal>>>,
}

impl Registry {
    pub fn create(
        &self,
        name: String,
        group: String,
        cwd: String,
        command: String,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalInfo> {
        let terminal = Terminal::spawn(name, group, cwd, command, cols, rows)?;
        let info = terminal.info();
        self.terminals.lock().unwrap().push(terminal);
        Ok(info)
    }

    pub fn list(&self) -> Vec<TerminalInfo> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .map(|terminal| terminal.info())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .find(|terminal| terminal.id == id)
            .map(Arc::clone)
    }

    /// Renomeia e/ou move de pasta. Campo ausente fica como está.
    pub fn update(&self, id: &str, name: Option<String>, group: Option<String>) -> Option<TerminalInfo> {
        let terminal = self.get(id)?;
        if let Some(name) = name {
            terminal.rename(name);
        }
        if let Some(group) = group {
            terminal.set_group(group);
        }
        Some(terminal.info())
    }

    /// Renomeia uma pasta inteira: todos os terminais dela passam a apontar
    /// para o nome novo. Devolve quantos foram movidos.
    pub fn rename_group(&self, from: &str, to: &str) -> usize {
        let mut moved = 0;
        for terminal in self.terminals.lock().unwrap().iter() {
            if terminal.group() == from {
                terminal.set_group(to.to_string());
                moved += 1;
            }
        }
        moved
    }

    /// As pastas existentes, na ordem em que aparecem.
    pub fn groups(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for terminal in self.terminals.lock().unwrap().iter() {
            let group = terminal.group();
            if !seen.contains(&group) {
                seen.push(group);
            }
        }
        seen
    }

    /// Mata o processo e tira o terminal da sidebar.
    pub fn remove(&self, id: &str) -> Option<()> {
        let terminal = self.get(id)?;
        let _ = terminal.kill();
        self.terminals
            .lock()
            .unwrap()
            .retain(|candidate| candidate.id != id);
        Some(())
    }
}
