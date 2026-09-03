use anyhow::{anyhow, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;
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

/// Tamanho de leitura do PTY. Blocos maiores reduzem troca de contexto.
const READ_CHUNK: usize = 8 * 1024;

/// O que a sidebar precisa saber sobre um terminal.
#[derive(Clone, Serialize)]
pub struct TerminalInfo {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub command: String,
    pub alive: bool,
}

/// Um terminal vivo: o PTY, o processo filho e o scrollback que permite reatar.
pub struct Terminal {
    pub id: String,
    name: Mutex<String>,
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

        // O comando chega como linha única; o primeiro token é o executável.
        let mut parts = command.split_whitespace();
        let program = parts.next().ok_or_else(|| anyhow!("comando vazio"))?;
        let mut builder = CommandBuilder::new(program);
        for arg in parts {
            builder.arg(arg);
        }
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
            cwd: self.cwd.clone(),
            command: self.command.clone(),
            alive: self.is_alive(),
        }
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
        cwd: String,
        command: String,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalInfo> {
        let terminal = Terminal::spawn(name, cwd, command, cols, rows)?;
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

    pub fn rename(&self, id: &str, name: String) -> Option<TerminalInfo> {
        let terminal = self.get(id)?;
        terminal.rename(name);
        Some(terminal.info())
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

/// Traduz um diretório de trabalho no slug que o Claude Code usa em
/// `~/.claude/projects/` — `C:\Users\Andre` vira `C--Users-Andre`.
pub fn claude_project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Descobre o id da sessão mais recente do Claude Code para um diretório.
///
/// É a mesma leitura que o slash command `/retomar` faz: o transcript mais novo
/// naquele projeto é a sessão que aquele terminal está usando.
pub fn latest_claude_session(cwd: &str) -> Option<String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    let directory = PathBuf::from(home)
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(cwd));

    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(directory).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Some(session_id) = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
        else {
            continue;
        };
        if newest
            .as_ref()
            .map_or(true, |(previous, _)| modified > *previous)
        {
            newest = Some((modified, session_id));
        }
    }

    newest.map(|(_, session_id)| session_id)
}
