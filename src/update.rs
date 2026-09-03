//! Atualização a partir do GitHub Releases, sem sair do programa.
//!
//! O fluxo é: consultar o release mais recente, comparar com a versão em
//! execução e, se houver novidade, baixar o binário do sistema atual e trocar o
//! executável em uso.
//!
//! ⚠️ **Repositório privado exige token.** Sem `SYNAPSE_DECK_GITHUB_TOKEN` no
//! ambiente, a consulta e o download só funcionam se o repositório for público.
//! Embutir um token no binário não é opção: quem tem o programa teria o token.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::Duration;

const REPO: &str = "dehstigliani/synapse-deck";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const TIMEOUT: Duration = Duration::from_secs(20);

/// O que a interface precisa saber sobre atualização.
#[derive(Debug, Default, Clone, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub available: bool,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    /// Motivo de não ter sido possível verificar — repositório privado sem
    /// token, sem rede, limite de requisições.
    pub error: Option<String>,
}

/// Nome do binário puro publicado para este sistema.
///
/// O updater baixa o executável direto, e não o arquivo compactado, para não
/// precisar de uma dependência de descompactação só para isto.
fn asset_name() -> &'static str {
    if cfg!(windows) {
        "synapse-deck-windows-x86_64.exe"
    } else if cfg!(target_os = "macos") {
        "synapse-deck-macos-arm64"
    } else {
        "synapse-deck-linux-x86_64"
    }
}

fn request(url: &str) -> ureq::Request {
    let mut request = ureq::get(url)
        .timeout(TIMEOUT)
        .set("User-Agent", "synapse-deck");
    if let Ok(token) = std::env::var("SYNAPSE_DECK_GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            request = request.set("Authorization", &format!("Bearer {}", token.trim()));
        }
    }
    request
}

/// Compara versões no formato `1.2.3` ou `1.2.3-alpha.4`.
///
/// Segue a regra do SemVer que importa aqui: um pré-lançamento vem **antes** da
/// versão final de mesmo número, e partes numéricas comparam por valor, não por
/// texto — senão `alpha.10` viria antes de `alpha.2`.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let split = |version: &str| {
        let version = version.trim_start_matches('v');
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_string())),
            None => (version, None),
        };
        let numbers: Vec<u64> = core
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        (numbers, pre)
    };

    let (left_numbers, left_pre) = split(left);
    let (right_numbers, right_pre) = split(right);

    for index in 0..left_numbers.len().max(right_numbers.len()) {
        let a = left_numbers.get(index).copied().unwrap_or(0);
        let b = right_numbers.get(index).copied().unwrap_or(0);
        if a != b {
            return a.cmp(&b);
        }
    }

    match (left_pre, right_pre) {
        (None, None) => Ordering::Equal,
        // Sem pré-lançamento é a versão final: vem depois.
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => {
            let parts = |text: &str| {
                text.split('.')
                    .map(|part| match part.parse::<u64>() {
                        Ok(number) => (0u8, number, String::new()),
                        Err(_) => (1u8, 0, part.to_string()),
                    })
                    .collect::<Vec<_>>()
            };
            parts(&a).cmp(&parts(&b))
        }
    }
}

/// Descobre o release mais novo, **incluindo pré-lançamentos**.
///
/// ⚠️ `/releases/latest` do GitHub ignora pré-lançamento e devolve 404 quando só
/// existem alphas — foi exatamente o que aconteceu aqui. Por isso a busca é na
/// lista e a escolha é por comparação de versão, não pela ordem da API.
fn newest_release() -> Result<Value> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=20");
    let releases: Value = request(&url)
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(404, _) => anyhow!(
                "repositório ou releases inacessíveis — se for privado, defina \
                 SYNAPSE_DECK_GITHUB_TOKEN"
            ),
            ureq::Error::Status(401 | 403, _) => anyhow!(
                "acesso negado ao repositório — defina SYNAPSE_DECK_GITHUB_TOKEN \
                 com um token que enxergue este repositório"
            ),
            other => anyhow!(other.to_string()),
        })?
        .into_json()
        .context("resposta ilegível do GitHub")?;

    let mut best: Option<(String, Value)> = None;
    for release in releases.as_array().cloned().unwrap_or_default() {
        if release.get("draft").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let tag = release
            .get("tag_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('v')
            .to_string();
        if tag.is_empty() {
            continue;
        }
        let melhor = match &best {
            Some((atual, _)) => compare_versions(&tag, atual) == Ordering::Greater,
            None => true,
        };
        if melhor {
            best = Some((tag, release));
        }
    }

    best.map(|(_, release)| release)
        .ok_or_else(|| anyhow!("nenhum release publicado"))
}

/// Consulta o release mais recente. Nunca falha: o erro vira campo da resposta.
pub fn check() -> UpdateStatus {
    let mut status = UpdateStatus {
        current: CURRENT_VERSION.to_string(),
        ..Default::default()
    };

    let release = match newest_release() {
        Ok(release) => release,
        Err(error) => {
            status.error = Some(error.to_string());
            return status;
        }
    };

    let latest = release
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_string();

    status.available = compare_versions(&latest, CURRENT_VERSION) == Ordering::Greater;
    status.notes = release
        .get("body")
        .and_then(Value::as_str)
        .map(|text| text.chars().take(1_500).collect());
    status.published_at = release
        .get("published_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    status.latest = Some(latest);
    status
}

/// Baixa a versão nova e troca o executável em uso.
///
/// No Windows não se sobrescreve um `.exe` em execução, mas se pode **renomeá-lo**:
/// o binário atual vira `.old`, o novo assume o lugar, e a próxima inicialização
/// apaga o antigo. O mesmo caminho serve nos outros sistemas.
///
/// A troca **não reinicia** o programa: reiniciar mataria todos os terminais
/// abertos. A versão nova entra na próxima inicialização.
pub fn apply() -> Result<PathBuf> {
    let release = newest_release()?;
    let wanted = asset_name();

    let asset = release
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|asset| asset.get("name").and_then(Value::as_str) == Some(wanted))
        })
        .ok_or_else(|| anyhow!("o release não traz o binário {wanted}"))?;

    // Em repositório privado o link público não serve: baixa-se pelo endpoint da
    // API, que aceita o token, pedindo o conteúdo bruto em vez do JSON do asset.
    let has_token = std::env::var("SYNAPSE_DECK_GITHUB_TOKEN")
        .map(|token| !token.trim().is_empty())
        .unwrap_or(false);

    let response = if has_token {
        let api_url = asset
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("asset sem url de API"))?;
        request(api_url)
            .set("Accept", "application/octet-stream")
            .call()
    } else {
        let public_url = asset
            .get("browser_download_url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("asset sem url de download"))?;
        request(public_url).call()
    }
    .context("falha ao baixar o binário novo")?;

    let mut bytes = Vec::new();
    std::io::copy(&mut response.into_reader(), &mut bytes).context("falha ao ler o download")?;
    if bytes.len() < 100_000 {
        return Err(anyhow!(
            "download suspeito: {} bytes, pequeno demais para o binário",
            bytes.len()
        ));
    }

    let current = std::env::current_exe().context("não consegui achar o executável em uso")?;
    let staged = current.with_extension("new");
    let previous = current.with_extension("old");

    std::fs::write(&staged, &bytes).context("falha ao gravar o binário novo")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }

    let _ = std::fs::remove_file(&previous);
    std::fs::rename(&current, &previous).context("não consegui afastar o binário em uso")?;
    if let Err(error) = std::fs::rename(&staged, &current) {
        // Desfaz para não deixar a instalação sem executável.
        let _ = std::fs::rename(&previous, &current);
        return Err(anyhow!("falha ao instalar o binário novo: {error}"));
    }

    Ok(current)
}

/// Apaga o binário antigo deixado por uma atualização anterior.
pub fn clean_previous() {
    if let Ok(current) = std::env::current_exe() {
        let _ = std::fs::remove_file(current.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_patch_wins() {
        assert_eq!(compare_versions("1.0.1", "1.0.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.9.9", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn release_beats_its_own_prerelease() {
        assert_eq!(compare_versions("1.0.0", "1.0.0-alpha.1"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0-alpha.1", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn prerelease_numbers_compare_by_value_not_text() {
        // O erro clássico: como texto, "10" viria antes de "2".
        assert_eq!(
            compare_versions("1.0.0-alpha.10", "1.0.0-alpha.2"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("1.0.0-alpha.2", "1.0.0-beta.1"),
            Ordering::Less
        );
    }

    #[test]
    fn leading_v_is_ignored() {
        assert_eq!(compare_versions("v1.2.0", "1.1.0"), Ordering::Greater);
    }
}
