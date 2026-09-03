//! Garante que mexer na interface refaz o binario.
//!
//! O `rust-embed` embute `web/` em tempo de compilacao, mas o cargo nao sabe
//! disso sozinho: sem esta linha, editar um arquivo la e recompilar pode gerar
//! um binario com a interface antiga, sem nenhum aviso. Num pipeline de release
//! isso e um bug silencioso caro.
fn main() {
    println!("cargo:rerun-if-changed=web");
}
