# rust-pty-terminal-workspace

Workspace de terminais nomeados com PTY em Rust, integrado ao Claude Code.

Projeto pessoal. Zona 🎓 **escola** — o retorno é aprender Rust e a camada de PTY.
Não é produto e não tem receita prevista.

> O nome é de protótipo e deve ser renomeado. Renomear no GitHub mantém redirect do
> nome antigo; só é preciso rodar `git remote set-url` no clone depois.

## O que faz

- Vários terminais simultâneos, cada um num PTY de verdade
- **Nomes** dados por você, editáveis com duplo clique na sidebar
- Sidebar com a lista e o estado de cada processo (rodando / encerrado)
- Um clique para criar terminal de **Claude** ou de **shell**
- **Os processos vivem no daemon**: fechar o browser não mata a sessão, e ao voltar
  o scrollback é reproduzido
- Detecta em qual **sessão do Claude Code** cada terminal está e entrega o
  `claude --resume <id>` pronto para copiar

## Rodar

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
| `GET` | `/api/terminals/:id/claude-session` | sessão do Claude detectada + comando de resume |
| `WS` | `/ws/:id` | liga ao PTY: replay do scrollback, depois ao vivo |

## Estado

⚠️ **Esqueleto ainda não compilado** — escrito antes de o toolchain Rust existir na
máquina. Os erros de compilação da primeira build entram num commit seguinte.

Fora do recorte por decisão, não por esquecimento:

- Empacotamento desktop (Tauri) — o browser serve; entra só se não servir
- Layouts e painéis divididos — 1 terminal ativo por vez basta para começar
- Persistência do workspace entre reinícios do daemon
- Autenticação — não faz sentido em loopback
