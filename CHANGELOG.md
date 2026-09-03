# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/),
versionamento [SemVer](https://semver.org/lang/pt-BR/).

## [1.0.0-alpha.1] — 2026-09-03

Primeira versão pública. **Alpha:** roda e faz o que promete, mas a superfície
ainda pode mudar sem aviso.

### Adicionado

- **Vários terminais nomeados**, cada um num PTY de verdade (ConPTY no Windows).
- **Os processos vivem no daemon.** Fechar o browser não mata a sessão; ao voltar,
  o scrollback é reproduzido. É a razão de existir do projeto.
- **Pastas** na sidebar para organizar os terminais, com grupo recolhível e
  edição de nome no lugar (duplo clique).
- **Claude Code integrado**: um clique sobe um terminal com o agente já rodando,
  e a pasta é marcada como confiável antes para não travar no
  "do you trust this folder".
- **Histórico de sessões** do diretório — título, data, nº de mensagens e tokens —
  com um clique para reabrir qualquer uma via `claude --resume`.
- **Sessões por boot**: agrupa o histórico pela inicialização da máquina em que
  cada sessão esteve viva. Histórico completo no Windows (evento 6005); Linux e
  macOS expõem apenas o boot atual.
- **Sessões ativas**: detecta conversas abertas fora do workspace e oferece
  adotá-las. Adotar reabre com `--resume` num processo novo — o sistema
  operacional não permite transferir o PTY de outro processo.
- **Painel de uso**: consumo agregado de todos os projetos, com totais, série por
  dia e ranking por projeto, aproveitamento de cache, fatia de raciocínio e custo
  equivalente de API. Tudo lido dos transcripts em disco, sem chamada de rede e
  sem tocar no token OAuth.
- **Medidor de contexto** por sessão, com a janela de 1M inferida pelo consumo
  observado (o transcript não distingue as duas pelo nome do modelo).
- **Três temas**: Orange Innovation (padrão), Dracula e Draculight.
- **Binário único**: a interface é embutida no executável, então o programa roda
  de qualquer diretório.

### Segurança e privacidade

- Escuta apenas em `127.0.0.1`. Nada é exposto na rede.
- Nenhum dado sai da máquina: o consumo vem dos arquivos locais.
- Os assets do xterm.js são versionados no repositório, não buscados em CDN — o
  programa funciona offline.
- A escrita em `~/.claude.json` (marcar pasta confiável) é atômica e faz backup
  antes da primeira alteração.

### Limitações conhecidas

- **Sem assinatura de código.** O Windows SmartScreen vai avisar na primeira
  execução, e o macOS não está notarizado.
- **Netos órfãos.** No Windows, comando que não é `.exe` roda sob `cmd.exe /C`;
  encerrar o terminal mata o `cmd`, e o processo neto pode sobreviver.
- **O workspace não persiste** entre reinícios do daemon: pastas e terminais são
  recriados do zero.
- **Adotar não transfere o processo**, apenas retoma a conversa.
- O scrollback guardado por terminal tem teto de 256 KB, em memória.
