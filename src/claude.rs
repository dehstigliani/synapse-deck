//! Tudo que este workspace sabe sobre o Claude Code: onde ele guarda as sessões,
//! quanto cada uma consumiu, e como marcar uma pasta como confiável.
//!
//! Nada aqui vai à rede. Todo o consumo é lido dos transcripts em disco — o que
//! dispensa mandar o token OAuth para endpoint nenhum.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// Tokens por hora do relógio, para o pulso da navbar.
    ///
    /// Fica fora do JSON: é detalhe interno, e serializar isso em toda listagem
    /// de sessão engordaria a resposta à toa.
    #[serde(skip)]
    pub hourly: Vec<(u64, u64)>,
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
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let content = fs::read_to_string(path).ok()?;
    Some(parse_session(id, modified, &content))
}

/// O miolo da leitura, separado do disco para poder ser testado.
pub fn parse_session(id: String, modified: u64, content: &str) -> SessionSummary {
    let mut title = String::new();
    let mut last_message = String::new();
    let mut message_count = 0usize;
    let mut usage = SessionUsage::default();
    let mut peak_context = 0u64;
    let mut hourly: HashMap<u64, u64> = HashMap::new();

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

                // Balde da hora cheia em que a resposta foi gravada.
                if let Some(at) = entry
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_timestamp)
                {
                    let processed = context + number("output_tokens");
                    *hourly.entry(at / 3_600 * 3_600).or_insert(0) += processed;
                }
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

    SessionSummary {
        hourly: hourly.into_iter().collect(),
        resume_command: format!("claude --resume {id}"),
        id,
        title: truncate(&title, 120),
        last_message: truncate(&last_message, 120),
        modified,
        message_count,
        usage,
    }
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

// ============================================================ preço e custo

/// Preço em dólar por milhão de tokens. Fonte: tabela oficial de modelos.
struct ModelPricing {
    input: f64,
    output: f64,
}

/// Cache lido custa 0,1× o preço de entrada; cache escrito custa 1,25× (TTL 5min).
const CACHE_READ_MULTIPLIER: f64 = 0.1;
const CACHE_WRITE_MULTIPLIER: f64 = 1.25;

fn pricing_for(model: &str) -> ModelPricing {
    let model = model.to_ascii_lowercase();
    if model.contains("fable") || model.contains("mythos") {
        ModelPricing { input: 10.0, output: 50.0 }
    } else if model.contains("haiku") {
        ModelPricing { input: 1.0, output: 5.0 }
    } else if model.contains("sonnet-5") {
        ModelPricing { input: 2.0, output: 10.0 }
    } else if model.contains("sonnet") {
        ModelPricing { input: 3.0, output: 15.0 }
    } else {
        // Família Opus, e o palpite seguro para modelo desconhecido.
        ModelPricing { input: 5.0, output: 25.0 }
    }
}

/// Custo em dólar equivalente, como se aquele consumo tivesse passado pela API.
///
/// ⚠️ Assinatura (Max/Pro) não cobra por token — este número é **referência de
/// grandeza**, não fatura. Serve para comparar sessões e projetos entre si.
fn cost_of(usage: &SessionUsage) -> f64 {
    let price = pricing_for(usage.model.as_deref().unwrap_or("claude-opus-5"));
    let million = 1_000_000.0;
    (usage.input_tokens as f64 * price.input
        + usage.output_tokens as f64 * price.output
        + usage.cache_read_tokens as f64 * price.input * CACHE_READ_MULTIPLIER
        + usage.cache_creation_tokens as f64 * price.input * CACHE_WRITE_MULTIPLIER)
        / million
}

// ============================================================ cache de leitura

/// Transcript já lido, guardado pela data de modificação do arquivo.
///
/// Sem isto, cada abertura do painel releria centenas de megabytes de `.jsonl`.
static SESSION_CACHE: Mutex<Option<HashMap<PathBuf, (u64, SessionSummary)>>> = Mutex::new(None);

fn read_session_cached(path: &Path) -> Option<SessionSummary> {
    let modified = fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();

    {
        let mut guard = SESSION_CACHE.lock().unwrap();
        let cache = guard.get_or_insert_with(HashMap::new);
        if let Some((cached_at, summary)) = cache.get(path) {
            if *cached_at == modified {
                return Some(summary.clone());
            }
        }
    }

    let summary = read_session(path)?;
    SESSION_CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(path.to_path_buf(), (modified, summary.clone()));
    Some(summary)
}

// ============================================================ varredura geral

/// Todos os diretórios de projeto do Claude Code.
fn all_project_directories() -> Vec<PathBuf> {
    let Some(home) = home_directory() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(home.join(".claude").join("projects")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn transcripts_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect()
}

// ============================================================ relatório de uso

#[derive(Debug, Default, Clone, Serialize)]
pub struct UsageTotals {
    pub sessions: usize,
    pub projects: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub thinking_tokens: u64,
    /// Fração da entrada que veio do cache — quanto o cache está economizando.
    pub cache_hit_ratio: f64,
    /// Fração da saída gasta raciocinando.
    pub thinking_ratio: f64,
    pub cost_usd_equivalent: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyUsage {
    pub date: String,
    pub sessions: usize,
    pub tokens: u64,
    pub cost_usd_equivalent: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsage {
    pub slug: String,
    pub sessions: usize,
    pub tokens: u64,
    pub cost_usd_equivalent: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    pub totals: UsageTotals,
    pub by_day: Vec<DailyUsage>,
    pub by_project: Vec<ProjectUsage>,
}

/// Dias desde a época a partir da data civil (Howard Hinnant) — o inverso de `day_of`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Converte `2026-09-03T17:16:17.679Z` em segundos desde a época.
///
/// O Claude Code sempre grava em UTC com esse formato fixo, então a leitura por
/// posição é suficiente e evita trazer uma biblioteca de data só para isto.
fn parse_timestamp(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let number = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let seconds = days_from_civil(number(0, 4)?, number(5, 7)?, number(8, 10)?) * 86_400
        + number(11, 13)? * 3_600
        + number(14, 16)? * 60
        + number(17, 19)?;
    u64::try_from(seconds).ok()
}

/// Data civil a partir do epoch, sem dependência externa (Howard Hinnant).
fn day_of(epoch_seconds: u64) -> String {
    let days = (epoch_seconds / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Varre todos os projetos e devolve o consumo agregado.
pub fn usage_report(days: usize) -> UsageReport {
    let mut totals = UsageTotals::default();
    let mut per_day: HashMap<String, (usize, u64, f64)> = HashMap::new();
    let mut per_project: Vec<ProjectUsage> = Vec::new();

    for directory in all_project_directories() {
        let slug = directory
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut project = ProjectUsage {
            slug,
            sessions: 0,
            tokens: 0,
            cost_usd_equivalent: 0.0,
        };

        for transcript in transcripts_in(&directory) {
            let Some(session) = read_session_cached(&transcript) else {
                continue;
            };
            let usage = &session.usage;
            let tokens = usage.input_tokens
                + usage.output_tokens
                + usage.cache_creation_tokens
                + usage.cache_read_tokens;
            let cost = cost_of(usage);

            totals.sessions += 1;
            totals.input_tokens += usage.input_tokens;
            totals.output_tokens += usage.output_tokens;
            totals.cache_read_tokens += usage.cache_read_tokens;
            totals.cache_creation_tokens += usage.cache_creation_tokens;
            totals.thinking_tokens += usage.thinking_tokens;
            totals.cost_usd_equivalent += cost;

            project.sessions += 1;
            project.tokens += tokens;
            project.cost_usd_equivalent += cost;

            let entry = per_day.entry(day_of(session.modified)).or_insert((0, 0, 0.0));
            entry.0 += 1;
            entry.1 += tokens;
            entry.2 += cost;
        }

        if project.sessions > 0 {
            totals.projects += 1;
            per_project.push(project);
        }
    }

    let total_input = totals.input_tokens + totals.cache_read_tokens + totals.cache_creation_tokens;
    if total_input > 0 {
        totals.cache_hit_ratio = totals.cache_read_tokens as f64 / total_input as f64;
    }
    if totals.output_tokens > 0 {
        totals.thinking_ratio = totals.thinking_tokens as f64 / totals.output_tokens as f64;
    }

    let mut by_day: Vec<DailyUsage> = per_day
        .into_iter()
        .map(|(date, (sessions, tokens, cost))| DailyUsage {
            date,
            sessions,
            tokens,
            cost_usd_equivalent: cost,
        })
        .collect();
    by_day.sort_by(|a, b| b.date.cmp(&a.date));
    by_day.truncate(days);
    by_day.reverse();

    per_project.sort_by(|a, b| b.tokens.cmp(&a.tokens));
    per_project.truncate(12);

    UsageReport {
        totals,
        by_day,
        by_project: per_project,
    }
}

// ============================================================ sessões por boot

#[derive(Debug, Clone, Serialize)]
pub struct BootGroup {
    /// Momento em que o Windows subiu.
    pub boot_at: u64,
    pub label: String,
    pub sessions: Vec<SessionSummary>,
}

/// Momentos de boot da máquina, do mais recente para o mais antigo.
///
/// Só o Windows entrega **histórico**: o evento 6005 do log de sistema marca
/// cada inicialização. Linux e macOS expõem apenas o boot atual, então lá a
/// lista tem no máximo um item — e a aba Boot vira "desde que a máquina ligou".
/// O resultado fica em cache por 15 minutos porque a consulta é lenta.
fn boot_times(limit: usize) -> Vec<u64> {
    static BOOT_CACHE: Mutex<Option<(u64, Vec<u64>)>> = Mutex::new(None);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    if let Some((cached_at, boots)) = BOOT_CACHE.lock().unwrap().as_ref() {
        if now.saturating_sub(*cached_at) < 900 {
            return boots.clone();
        }
    }

    let boots = read_boot_times(limit);
    *BOOT_CACHE.lock().unwrap() = Some((now, boots.clone()));
    boots
}

/// Windows guarda o histórico de inicializações no log de eventos.
#[cfg(windows)]
fn read_boot_times(limit: usize) -> Vec<u64> {
    let script = format!(
        "Get-WinEvent -FilterHashtable @{{LogName='System';ID=6005}} -MaxEvents {limit} -ErrorAction SilentlyContinue | ForEach-Object {{ [DateTimeOffset]::new($_.TimeCreated.ToUniversalTime(), [TimeSpan]::Zero).ToUnixTimeSeconds() }}"
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.trim().parse::<u64>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// O Linux publica o instante do boot atual em `/proc/stat`, na linha `btime`.
/// Não há histórico: a lista tem no máximo um item.
#[cfg(target_os = "linux")]
fn read_boot_times(_limit: usize) -> Vec<u64> {
    fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|content| {
            content
                .lines()
                .find_map(|line| line.strip_prefix("btime ")?.trim().parse::<u64>().ok())
        })
        .into_iter()
        .collect()
}

/// No macOS o boot atual sai do `sysctl kern.boottime`, no formato
/// `{ sec = 1788393600, usec = 0 } ...`. Também sem histórico.
#[cfg(target_os = "macos")]
fn read_boot_times(_limit: usize) -> Vec<u64> {
    std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()
        .and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let start = text.find("sec = ")? + 6;
            let rest = &text[start..];
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            rest[..end].parse::<u64>().ok()
        })
        .into_iter()
        .collect()
}

/// Agrupa as sessões pelo boot do Windows em que estavam vivas.
pub fn sessions_by_boot(limit: usize) -> Vec<BootGroup> {
    let boots = boot_times(limit.max(1));
    if boots.is_empty() {
        return Vec::new();
    }

    let mut sessions: Vec<SessionSummary> = all_project_directories()
        .iter()
        .flat_map(|directory| transcripts_in(directory))
        .filter_map(|path| read_session_cached(&path))
        .collect();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));

    let mut groups: Vec<BootGroup> = boots
        .iter()
        .map(|boot_at| BootGroup {
            boot_at: *boot_at,
            label: day_of(*boot_at),
            sessions: Vec::new(),
        })
        .collect();

    // Cada sessão cai no boot mais recente que a precede.
    for session in sessions {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| session.modified >= group.boot_at)
        {
            group.sessions.push(session);
        }
    }

    groups.retain(|group| !group.sessions.is_empty());
    groups
}

// ============================================================ sessões ativas

/// Sessões cujo transcript foi escrito há pouco — quase certamente ainda abertas
/// em algum terminal fora deste workspace.
///
/// ⚠️ Não dá para adotar o processo alheio: o sistema operacional não entrega o
/// PTY de outro processo. O que o workspace faz é reabrir a conversa com
/// `--resume`, continuando a sessão num processo novo; o terminal antigo deve
/// ser fechado por quem o abriu.
pub fn active_sessions(within_seconds: u64) -> Vec<SessionSummary> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    let mut sessions: Vec<SessionSummary> = all_project_directories()
        .iter()
        .flat_map(|directory| transcripts_in(directory))
        .filter_map(|path| read_session_cached(&path))
        .filter(|session| now.saturating_sub(session.modified) <= within_seconds)
        .collect();

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}



/// O pulso de consumo: quanto foi gasto agora, na janela de limite e na semana.
#[derive(Debug, Default, Clone, Serialize)]
pub struct UsagePulse {
    pub tokens_last_hour: u64,
    /// A janela de 5 horas é o ciclo de limite das assinaturas.
    pub tokens_last_5h: u64,
    pub tokens_last_week: u64,
    /// Os picos já registrados, que servem de denominador para as porcentagens.
    ///
    /// ⚠️ Nenhum deles é a cota do plano — esse número não existe em disco. As
    /// porcentagens comparam o ritmo de agora com o pico do próprio usuário.
    pub peak_hour_tokens: u64,
    pub peak_5h_tokens: u64,
    pub peak_week_tokens: u64,
    pub hour_percent: f64,
    pub five_hour_percent: f64,
    pub week_percent: f64,
}

/// Maior soma de uma janela deslizante sobre uma série ordenada de baldes.
///
/// Os baldes vazios contam como zero: uma pausa no meio da janela não pode
/// inflar o pico juntando dois picos separados por dias de silêncio.
fn peak_window(buckets: &HashMap<u64, u64>, bucket_size: u64, window: u64) -> u64 {
    if buckets.is_empty() {
        return 0;
    }
    let mut ordered: Vec<(u64, u64)> = buckets.iter().map(|(at, n)| (*at, *n)).collect();
    ordered.sort_by_key(|(at, _)| *at);

    let mut peak = 0u64;
    let mut start = 0usize;
    let mut running = 0u64;
    for index in 0..ordered.len() {
        running += ordered[index].1;
        // Encolhe pela esquerda enquanto a janela for maior que o permitido.
        while ordered[index].0.saturating_sub(ordered[start].0) >= window {
            running -= ordered[start].1;
            start += 1;
        }
        let _ = bucket_size;
        peak = peak.max(running);
    }
    peak
}

pub fn usage_pulse() -> UsagePulse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    let mut pulse = UsagePulse::default();
    let mut totals_by_hour: HashMap<u64, u64> = HashMap::new();

    for directory in all_project_directories() {
        for transcript in transcripts_in(&directory) {
            let Some(session) = read_session_cached(&transcript) else {
                continue;
            };
            for (hour, tokens) in session.hourly {
                *totals_by_hour.entry(hour).or_insert(0) += tokens;
            }
        }
    }

    for (hour, tokens) in &totals_by_hour {
        let age = now.saturating_sub(*hour);
        if age <= 3_600 {
            pulse.tokens_last_hour += tokens;
        }
        if age <= 5 * 3_600 {
            pulse.tokens_last_5h += tokens;
        }
        if age <= 7 * 86_400 {
            pulse.tokens_last_week += tokens;
        }
        pulse.peak_hour_tokens = pulse.peak_hour_tokens.max(*tokens);
    }

    pulse.peak_5h_tokens = peak_window(&totals_by_hour, 3_600, 5 * 3_600);
    pulse.peak_week_tokens = peak_window(&totals_by_hour, 3_600, 7 * 86_400);

    let ratio = |part: u64, whole: u64| {
        if whole == 0 {
            0.0
        } else {
            (part as f64 / whole as f64 * 100.0).min(100.0)
        }
    };
    pulse.hour_percent = ratio(pulse.tokens_last_hour, pulse.peak_hour_tokens);
    pulse.five_hour_percent = ratio(pulse.tokens_last_5h, pulse.peak_5h_tokens);
    pulse.week_percent = ratio(pulse.tokens_last_week, pulse.peak_week_tokens);
    pulse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_layout() {
        assert_eq!(
            project_slug("C:\\Users\\AndreStigliani"),
            "C--Users-AndreStigliani"
        );
    }

    #[test]
    fn civil_date_from_epoch() {
        assert_eq!(day_of(0), "1970-01-01");
        // 2026-09-03T00:00:00Z — conferido contra o calendário, não estimado.
        assert_eq!(day_of(1_788_393_600), "2026-09-03");
        // A véspera do mesmo instante, para pegar erro de arredondamento de dia.
        assert_eq!(day_of(1_788_393_599), "2026-09-02");
    }

    #[test]
    fn timestamp_round_trips_with_day_of() {
        let at = parse_timestamp("2026-09-03T17:16:17.679Z").expect("data válida");
        assert_eq!(day_of(at), "2026-09-03");
        // Meia-noite em UTC tem que bater exatamente com o epoch conferido.
        assert_eq!(parse_timestamp("2026-09-03T00:00:00.000Z"), Some(1_788_393_600));
        assert_eq!(parse_timestamp("nao e data"), None);
    }

    #[test]
    fn cost_uses_cache_multipliers() {
        let usage = SessionUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_creation_tokens: 1_000_000,
            model: Some("claude-opus-5".to_string()),
            ..Default::default()
        };
        // 5 (entrada) + 25 (saída) + 0,5 (cache lido) + 6,25 (cache escrito)
        assert!((cost_of(&usage) - 36.75).abs() < 1e-9);
    }

    fn fixture() -> String {
        [
            r#"{"type":"user","message":{"content":"primeira pergunta"}}"#,
            r#"{"type":"user","message":{"content":"<system-reminder>ignorar</system-reminder>"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":300000,"cache_creation_input_tokens":100,"output_tokens_details":{"thinking_tokens":7}}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"segunda pergunta"}]}}"#,
        ]
        .join("\n")
    }

    #[test]
    fn parses_title_and_skips_injected_messages() {
        let session = parse_session("abc".to_string(), 0, &fixture());
        assert_eq!(session.title, "primeira pergunta");
        assert_eq!(session.last_message, "segunda pergunta");
        // A mensagem que começa com "<" não conta como conversa.
        assert_eq!(session.message_count, 2);
        assert_eq!(session.resume_command, "claude --resume abc");
    }

    #[test]
    fn sums_usage_and_infers_extended_window() {
        let session = parse_session("abc".to_string(), 0, &fixture());
        let usage = &session.usage;
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.thinking_tokens, 7);
        assert_eq!(usage.model.as_deref(), Some("claude-opus-5"));
        // O contexto vivo é o prompt da última resposta.
        assert_eq!(usage.context_tokens, 10 + 300_000 + 100);
        // Passou de 200k, logo a janela só pode ser a estendida.
        assert_eq!(usage.context_limit, EXTENDED_CONTEXT_LIMIT);
    }

    #[test]
    fn peak_window_ignores_gaps() {
        let mut buckets = HashMap::new();
        // Dois picos de 100 separados por 10 horas não podem virar um de 200.
        buckets.insert(0u64, 100u64);
        buckets.insert(36_000u64, 100u64);
        // Duas horas seguidas somam de verdade.
        buckets.insert(72_000u64, 30u64);
        buckets.insert(75_600u64, 40u64);
        assert_eq!(peak_window(&buckets, 3_600, 5 * 3_600), 100);
        assert_eq!(peak_window(&buckets, 3_600, 7 * 86_400), 270);
        assert_eq!(peak_window(&HashMap::new(), 3_600, 3_600), 0);
    }

    #[test]
    fn empty_transcript_does_not_panic() {
        let session = parse_session("vazia".to_string(), 0, "");
        assert_eq!(session.message_count, 0);
        assert_eq!(session.usage.context_limit, DEFAULT_CONTEXT_LIMIT);
    }
}
