//! Preparação de compilação.
//!
//! Duas coisas: garantir que mexer na interface refaz o binário, e dar cara de
//! programa ao executável no Windows — ícone e propriedades do arquivo.

fn main() {
    // O `rust-embed` embute `web/` em tempo de compilação, mas o cargo não sabe
    // disso sozinho: sem esta linha, editar um arquivo lá e recompilar pode
    // gerar um binário com a interface antiga, sem nenhum aviso. Num pipeline de
    // release isso é um bug silencioso caro.
    println!("cargo:rerun-if-changed=web");

    #[cfg(windows)]
    embed_windows_resources();
}

/// Ícone e metadados no `.exe`.
///
/// Sem isto o Windows mostra o ícone genérico de console no Explorer, na barra
/// de tarefas e no Alt+Tab, e a aba de Propriedades do arquivo fica vazia.
#[cfg(windows)]
fn embed_windows_resources() {
    println!("cargo:rerun-if-changed=web/assets/app.ico");

    let mut resources = winresource::WindowsResource::new();
    // `app.ico` traz arte por tamanho: em 16 e 24 px só o raio, que é o
    // que sobrevive; a marca inteira vira mancha nesse tamanho.
    resources.set_icon("web/assets/app.ico");
    resources.set("ProductName", "Synapse Deck");
    resources.set("FileDescription", "Synapse Deck — o deck de terminais e agentes");
    resources.set("CompanyName", "André Stigliani");
    resources.set("LegalCopyright", "© 2026 André Stigliani — MIT");
    resources.set("OriginalFilename", "synapse-deck.exe");

    // Falhar aqui não pode derrubar a compilação: quem monta o binário sem o
    // SDK de recursos do Windows ainda tem um programa funcional, só sem ícone.
    if let Err(error) = resources.compile() {
        println!("cargo:warning=ícone do Windows não embutido: {error}");
    }
}
