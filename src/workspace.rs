//! Persistência do workspace entre execuções.
//!
//! O daemon segura os processos, mas quando ele mesmo é encerrado — reinício da
//! máquina, atualização, ou você fechando o Deck — os PTYs morrem junto. O que
//! sobrevive é a **conversa**: o Claude Code grava os transcripts em disco.
//!
//! Então o que se guarda aqui é a *forma* do workspace — quais terminais, com
//! que nome, em que pasta e em que diretório — mais o id da sessão de cada
//! terminal de agente. Ao abrir de novo, os terminais são recriados e os de
//! Claude sobem com `--resume`, continuando de onde pararam.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::claude;
use crate::terminal::Registry;

/// Um terminal como ele será recriado na próxima abertura.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedTerminal {
    pub name: String,
    pub group: String,
    pub cwd: String,
    pub command: String,
    /// Sessão do Claude a retomar, quando o terminal é de agente.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SavedWorkspace {
    version: u32,
    terminals: Vec<SavedTerminal>,
}

/// Onde o arquivo mora.
///
/// Fica no diretório de configuração do usuário, e não ao lado do executável,
/// porque o atualizador troca o binário e o instalador reescreve a pasta de
/// instalação — o workspace tem que sobreviver aos dois.
fn workspace_path() -> Option<PathBuf> {
    let base = std::env::var("APPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("XDG_CONFIG_HOME")
                .ok()
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("synapse-deck").join("workspace.json"))
}

/// Um terminal é de agente? Só esses ganham `--resume`.
fn is_agent(command: &str) -> bool {
    command.contains("claude")
}

/// Grava o estado atual. Chamado quando um terminal nasce, muda ou morre.
///
/// A escrita é minúscula e por evento — nada aqui se aproxima do padrão de
/// gravação contínua que se evita no scrollback.
pub fn save(registry: &Arc<Registry>) -> Result<()> {
    let path = workspace_path().context("sem diretório de configuração")?;
    std::fs::create_dir_all(path.parent().context("caminho sem pasta")?)?;

    let terminals: Vec<SavedTerminal> = registry
        .list()
        .into_iter()
        .map(|info| SavedTerminal {
            // A sessão é resolvida na hora de salvar: no momento da criação ela
            // ainda não existe em disco.
            //
            // ⚠️ Com dois terminais de agente na mesma pasta, ambos gravam a
            // sessão mais recente dali e voltam apontando para a mesma conversa.
            session_id: if is_agent(&info.command) {
                claude::latest_session(&info.cwd).map(|session| session.id)
            } else {
                None
            },
            name: info.name,
            group: info.group,
            cwd: info.cwd,
            command: info.command,
        })
        .collect();

    let saved = SavedWorkspace {
        version: 1,
        terminals,
    };

    // Escrita atômica: um desligamento no meio não pode deixar JSON pela metade.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_string_pretty(&saved)?)?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

/// Recria os terminais da última sessão. Devolve quantos subiram.
pub fn restore(registry: &Arc<Registry>) -> usize {
    let Some(path) = workspace_path() else {
        return 0;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(saved) = serde_json::from_str::<SavedWorkspace>(&content) else {
        return 0;
    };

    let mut restored = 0;
    for terminal in saved.terminals {
        // Pasta que sumiu desde a última vez não deve derrubar a restauração
        // inteira — o terminal simplesmente não volta.
        if !std::path::Path::new(&terminal.cwd).is_dir() {
            continue;
        }

        let command = match (&terminal.session_id, is_agent(&terminal.command)) {
            (Some(session), true) => format!("claude --resume {session}"),
            _ => terminal.command.clone(),
        };

        if is_agent(&command) {
            let _ = claude::trust_directory(&terminal.cwd);
        }

        if registry
            .create(
                terminal.name,
                terminal.group,
                terminal.cwd,
                command,
                120,
                30,
            )
            .is_ok()
        {
            restored += 1;
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_agent_terminals_get_resume() {
        assert!(is_agent("claude"));
        assert!(is_agent("claude --resume abc"));
        assert!(!is_agent(r"C:\WINDOWS\system32\cmd.exe"));
        assert!(!is_agent("/bin/bash"));
    }

    #[test]
    fn saved_shape_survives_a_round_trip() {
        let saved = SavedWorkspace {
            version: 1,
            terminals: vec![SavedTerminal {
                name: "deck".into(),
                group: "Synapse Deck".into(),
                cwd: "C:/tmp".into(),
                command: "claude".into(),
                session_id: Some("abc-123".into()),
            }],
        };
        let json = serde_json::to_string(&saved).expect("serializa");
        let back: SavedWorkspace = serde_json::from_str(&json).expect("desserializa");
        assert_eq!(back.terminals.len(), 1);
        assert_eq!(back.terminals[0].session_id.as_deref(), Some("abc-123"));
        assert_eq!(back.terminals[0].group, "Synapse Deck");
    }

    #[test]
    fn missing_session_id_is_tolerated() {
        // Arquivo gravado por uma versão anterior, sem o campo.
        let json = r#"{"version":1,"terminals":[
            {"name":"a","group":"g","cwd":"C:/tmp","command":"cmd.exe"}]}"#;
        let back: SavedWorkspace = serde_json::from_str(json).expect("campo opcional");
        assert!(back.terminals[0].session_id.is_none());
    }
}
