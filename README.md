# Synapse Deck

**O deck de terminais e agentes.** Vários terminais nomeados, cada um num PTY de
verdade, com o Claude Code rodando dentro e sobrevivendo ao fechar da janela.

## O que faz

- Vários terminais simultâneos, cada um num PTY de verdade
- **Nomes** dados por você, editáveis com duplo clique na sidebar
- Sidebar com a lista e o estado de cada processo (rodando / encerrado)
- Um clique para criar terminal de **Claude** ou de **shell**
- **Os processos vivem no daemon**: fechar o browser não mata a sessão, e ao voltar
  o scrollback é reproduzido
- **Pastas** na sidebar: agrupa terminais, recolhe grupo, move terminal de pasta
- **Histórico de sessões** do diretório: título, data, nº de mensagens e tokens de
  cada sessão do Claude Code, e um clique abre qualquer uma num terminal novo
- **Medidor de contexto**: quanto da janela a sessão já ocupa, em %
- **Temas**, começando pelo Dracula
- Marca a pasta como confiável antes de subir o Claude, para o terminal não nascer
  preso no "do you trust this folder"

## Instalar

Baixe o pacote do seu sistema em
[Releases](https://github.com/dehstigliani/synapse-deck/releases) e rode o binário.
Ele é autocontido — a interface vai embutida — então roda de qualquer diretório.

⚠️ Os binários **não são assinados**: o SmartScreen do Windows avisa na primeira
execução e o macOS não está notarizado.

## Rodar do código

```bash
cargo run
# abre http://127.0.0.1:7788
```

Só escuta em `127.0.0.1`. Nada deste workspace é exposto na rede — decisão
deliberada, ao contrário do Remote Control em HTTP claro que auditamos no Alethe.

## Arquitetura

```
browser (xterm.js)  ──WebSocket──>  daemon Rust (axum)  ──ConPTY──>  claude / shell
   sidebar + painel                  Registry<Terminal>              processo real
                                     scrollback em RAM
```

Três decisões que valem registro:

**1. Uma thread de SO por terminal.** Leitura de PTY é I/O bloqueante, não async.
A thread é o que mantém o processo sendo consumido mesmo sem browser conectado —
é ela que faz a sessão sobreviver à janela.

**2. Scrollback em memória, com teto de 256 KB e nada em disco.** É a lição direta
da issue #22 do Alethe: gravar cada byte do PTY em disco gerava ~27 MB/s contínuos,
~2,3 TB/dia, com reclamação de desgaste de SSD. Aqui o buffer é um anel com teto fixo.

**3. Assinar o canal antes de tirar o snapshot.** Na ordem inversa, os bytes que
chegam entre o snapshot e a assinatura são perdidos e a tela fica furada.

## API

| Método | Rota | O que faz |
|---|---|---|
| `GET` | `/api/terminals` | lista (alimenta a sidebar) |
| `POST` | `/api/terminals` | cria `{name, cwd?, command?, cols?, rows?}` |
| `GET` | `/api/terminals/:id` | detalhe de um terminal |
| `PATCH` | `/api/terminals/:id` | renomeia `{name}` |
| `DELETE` | `/api/terminals/:id` | mata o processo e remove |
| `GET` | `/api/terminals/:id/claude-session` | sessão atual + consumo (contexto, tokens, modelo) |
| `GET` | `/api/sessions?cwd=…` | histórico de sessões daquele diretório |
| `GET` | `/api/sessions/by-boot` | sessões agrupadas pelo boot do Windows |
| `GET` | `/api/sessions/active` | sessões escritas há pouco (abertas fora do workspace) |
| `GET` | `/api/usage?days=N` | consumo agregado: totais, por dia e por projeto |
| `GET` | `/api/groups` | as pastas existentes |
| `WS` | `/ws/:id` | liga ao PTY: replay do scrollback, depois ao vivo |

## De onde vem o "% de uso"

Do **transcript em disco**, não da rede. Cada resposta do Claude Code grava
`input_tokens`, `output_tokens`, `cache_read_input_tokens`,
`cache_creation_input_tokens` e `thinking_tokens` no `.jsonl` da sessão. O medidor
soma isso e calcula a ocupação da janela de contexto — exato, local, e sem
precisar do token OAuth.

A janela de 1M não dá para distinguir pelo nome do modelo (o transcript grava
`claude-opus-5` nos dois casos), então ela é inferida pelo consumo observado:
se a sessão já passou de 200k, a janela só pode ser a estendida.

⚠️ O que isto **não** dá é o **% da cota do plano** — esse denominador não está no
arquivo. Só a API do provedor tem, e o preço de buscá-la é mandar o Bearer token
para um endpoint não documentado a cada poucos minutos. Decisão consciente: fora.

## Sessões por boot, e adotar sessão aberta em outro terminal

**Por boot:** o evento 6005 do log de sistema do Windows marca cada inicialização.
A aba Boot lê esses eventos e joga cada sessão no boot mais recente que a precede —
é como reencontrar o que estava aberto antes de um reinício.

**Ativas:** transcript escrito nos últimos 15 minutos quase sempre significa sessão
aberta em algum terminal. É um indício, não certeza: o Claude Code pode tocar vários
arquivos de uma vez, e aí sessões paradas aparecem juntas na lista.

⚠️ **Adotar não é tomar o processo.** O sistema operacional não entrega o PTY de um
processo a outro — isso não é limitação de implementação, é como o SO funciona. O
que a adoção faz é reabrir a conversa aqui com `claude --resume`, num processo novo.
A janela antiga continua viva e deve ser fechada por quem a abriu.

## Armadilhas do Windows que já custaram tempo

**`CreateProcessW` só executa binário.** O `claude` instalado pelo npm é um script
(`claude`, `claude.cmd`, `claude.ps1` — nenhum é `.exe`), e o spawn direto falha com
`%1 não é um aplicativo Win32 válido` (os error 193). Por isso comando que não termina
em `.exe` passa pelo `%COMSPEC% /C`. Consequência conhecida: o filho do PTY vira o
`cmd.exe`, então `DELETE` mata o `cmd` e pode deixar o processo neto órfão.

**Sessão-filha herdada mata o transcript.** Se o daemon subir de dentro de um
Claude Code, ele herda `CLAUDE_CODE_CHILD_SESSION` (e mais 8 variáveis de sessão)
e repassa aos filhos — que então **não gravam transcript**, esvaziando o histórico
e o medidor. O spawn remove todas: um terminal do workspace é sessão de topo.

**Assets do xterm.js são versionados aqui, não vêm de CDN.** O pacote virou
`@xterm/xterm` (v6) e os caminhos antigos do cdnjs dão 404. Além disso, ferramenta
local que depende de CDN não funciona offline — `web/vendor/` resolve os dois.

## Estado

✅ **Compila e roda.** Verificado ponta a ponta em 03/09/2026:

- `cargo build` limpo (Rust 1.x, `stable-x86_64-pc-windows-msvc`)
- Terminal de shell e terminal de Claude subindo no mesmo workspace
- Claude Code renderizando com cores ANSI dentro do PTY, no browser
- Entrada de teclado chegando no processo pelo WebSocket
- Troca de terminal pela sidebar, com o ativo destacado
- **A aba do browser foi fechada por completo e reaberta: os dois processos
  continuaram vivos e o scrollback foi reproduzido inteiro** — a promessa central

Fora do recorte por decisão, não por esquecimento:

- Empacotamento desktop (Tauri) — o browser serve; entra só se não servir
- Layouts e painéis divididos — 1 terminal ativo por vez basta para começar
- Persistência do workspace (pastas e terminais) entre reinícios do daemon
- Autenticação — não faz sentido em loopback

## Barreira contra credencial

O repositório tem um hook que recusa commits com aparência de credencial —
token do GitHub, chave AWS, chave privada, atribuição a variável de senha. Ele
cobre o caso que o `.gitignore` não pega: o segredo colado no meio de um
arquivo legítimo.

Depois de clonar, ative com:

```bash
git config core.hooksPath scripts
```

Para um exemplo comprovadamente falso, `git commit --no-verify` e explique no
corpo do commit. Se um segredo real chegou a ir para o remoto, **revogue a
credencial**: apagar do código não desfaz o vazamento.
