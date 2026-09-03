//! Tudo que este workspace sabe sobre o Claude Code: onde ele guarda as sessões,
//! quanto cada uma consumiu, e como marcar uma pasta como confiável.
//!
//! Nada aqui vai à rede. Todo o consumo é lido dos transcripts em disco — o que
//! dispensa mandar o token OAuth para endpoint nenhum.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Janela de contexto padrão dos modelos Claude.
const DEFAULT_CONTEXT_LIMIT: u64 = 200_000;

/// Janela estendida. Não dá para distinguir pelo nome do modelo no transcript
/// (ambos gravam `claude-opus-5`), então a inferência é pelo próprio consumo:
/// se a sessão já passou do limite padrão, a janela só pode ser esta.
const EXTENDED_CONTEXT_LIMIT: u64 = 1_000_000;

/// Quanto uma sessão consumiu, somado do transcript.
#[derive(Debug, Default, Clone, Serialize)]
pub struct SessionUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub thinking_tokens: u64,
    /// Tamanho do último prompt: é o que ocupa a janela de contexto agora.
    pub context_tokens: u64,
    pub context_limit: u64,
    pub context_percent: f64,
    pub model: Option<String>,
}

/// Uma sessão do Claude Code encontrada em disco.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    /// Primeira mensagem do usuário — serve de título.
    pub title: String,
    pub last_message: String,
    pub modified: u64,
    pub message_count: usize,
    pub usage: SessionUsage,
    pub resume_command: String,
}

/// Traduz um diretório de trabalho no slug que o Claude Code usa em
/// `~/.claude/projects/` — `C:\Users\Andre` vira `C--Users-Andre`.
pub fn project_slug(cwd: &str) -> String {
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

fn home_directory() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
}

fn project_directory(cwd: &str) -> Option<PathBuf> {
    Some(
        home_directory()?
            .join(".claude")
            .join("projects")
            .join(project_slug(cwd)),
    )
}

/// Extrai o texto de um campo `content`, que ora é string, ora lista de blocos.
fn extract_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    for block in content.as_array()? {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// Mensagem do usuário que serve de título: ignora as injetadas pelo sistema.
fn is_readable_user_message(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && !trimmed.starts_with('<')
}

/// Lê um transcript inteiro e devolve o resumo da sessão.
pub fn read_session(path: &Path) -> Option<SessionSummary> {
    let id = path.file_stem()?.to_string_lossy().to_string();
    let modified = fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    let content = fs::read_to_string(path).ok()?;

    let mut title = String::new();
    let mut last_message = String::new();
    let mut message_count = 0usize;
    let mut usage = SessionUsage::default();
    let mut peak_context = 0u64;

    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        match entry.get("type").and_then(Value::as_str) {
            Some("user") => {
                let Some(text) = entry
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(extract_text)
                else {
                    continue;
                };
                if is_readable_user_message(&text) {
                    if title.is_empty() {
                        title = text.clone();
                    }
                    last_message = text;
                    message_count += 1;
                }
            }
            Some("assistant") => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if let Some(model) = message.get("model").and_then(Value::as_str) {
                    if model != "<synthetic>" {
                        usage.model = Some(model.to_string());
                    }
                }
                let Some(entry_usage) = message.get("usage") else {
                    continue;
                };
                let number = |key: &str| entry_usage.get(key).and_then(Value::as_u64).unwrap_or(0);

                let input = number("input_tokens");
                let cache_read = number("cache_read_input_tokens");
                let cache_creation = number("cache_creation_input_tokens");

                usage.input_tokens += input;
                usage.output_tokens += number("output_tokens");
                usage.cache_read_tokens += cache_read;
                usage.cache_creation_tokens += cache_creation;
                usage.thinking_tokens += entry_usage
                    .get("output_tokens_details")
                    .and_then(|details| details.get("thinking_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);

                // O contexto vivo é o prompt da última resposta, não a soma da sessão.
                let context = input + cache_read + cache_creation;
                usage.context_tokens = context;
                peak_context = peak_context.max(context);
            }
            _ => {}
        }
    }

    usage.context_limit = if peak_context > DEFAULT_CONTEXT_LIMIT {
        EXTENDED_CONTEXT_LIMIT
    } else {
        DEFAULT_CONTEXT_LIMIT
    };
    usage.context_percent =
        (usage.context_tokens as f64 / usage.context_limit as f64 * 100.0).min(100.0);

    Some(SessionSummary {
        resume_command: format!("claude --resume {id}"),
        id,
        title: truncate(&title, 120),
        last_message: truncate(&last_message, 120),
        modified,
        message_count,
        usage,
    })
}

fn truncate(text: &str, limit: usize) -> String {
    let single_line = text.replace(['\n', '\r'], " ");
    if single_line.chars().count() <= limit {
        return single_line;
    }
    single_line.chars().take(limit).collect::<String>() + "…"
}

/// Todas as sessões de um diretório, da mais recente para a mais antiga.
pub fn list_sessions(cwd: &str) -> Vec<SessionSummary> {
    let Some(directory) = project_directory(cwd) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut sessions: Vec<SessionSummary> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|path| read_session(&path))
        .collect();

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

/// A sessão mais recente de um diretório.
pub fn latest_session(cwd: &str) -> Option<SessionSummary> {
    list_sessions(cwd).into_iter().next()
}

/// Marca a pasta como confiável no `~/.claude.json`, para o Claude Code não
/// perguntar "do you trust this folder" ao abrir um terminal novo.
///
/// ⚠️ Este arquivo é escrito também pelas instâncias do Claude Code em execução.
/// Aqui a escrita é atômica (tmp + rename) e só acontece quando a chave falta,
/// mas uma corrida com outra instância continua teoricamente possível.
pub fn trust_directory(cwd: &str) -> Result<bool> {
    let home = home_directory().context("sem HOME/USERPROFILE")?;
    let config_path = home.join(".claude.json");
    if !config_path.exists() {
        return Ok(false);
    }

    let raw = fs::read_to_string(&config_path)?;
    let mut config: Value = serde_json::from_str(&raw)?;

    // O Claude Code grava o caminho com barra normal, e a letra do drive aparece
    // ora maiúscula ora minúscula conforme o shell que abriu. Cobre as duas.
    let normalized = cwd.replace('\\', "/");
    let mut variants = vec![normalized.clone()];
    if let Some(drive) = normalized.chars().next() {
        if drive.is_ascii_alphabetic() {
            let flipped = if drive.is_uppercase() {
                drive.to_ascii_lowercase()
            } else {
                drive.to_ascii_uppercase()
            };
            variants.push(format!("{flipped}{}", &normalized[1..]));
        }
    }

    let projects = config
        .get_mut("projects")
        .and_then(Value::as_object_mut)
        .context("~/.claude.json sem a chave projects")?;

    let mut changed = false;
    for variant in variants {
        let entry = projects
            .entry(variant)
            .or_insert_with(|| serde_json::json!({}));
        let already = entry
            .get("hasTrustDialogAccepted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !already {
            entry["hasTrustDialogAccepted"] = Value::Bool(true);
            changed = true;
        }
    }

    if !changed {
        return Ok(false);
    }

    // Backup uma vez só, antes da primeira escrita nossa.
    let backup_path = home.join(".claude.json.workspace-backup");
    if !backup_path.exists() {
        fs::write(&backup_path, &raw)?;
    }

    let temporary = config_path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_string(&config)?)?;
    fs::rename(&temporary, &config_path)?;
    Ok(true)
}
