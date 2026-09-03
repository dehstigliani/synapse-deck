# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/),
versionamento [SemVer](https://semver.org/lang/pt-BR/).

## [1.0.0-alpha.5] — 2026-09-03

### Corrigido

- **Não era um programa desktop.** Clicar no atalho abria uma janela preta de
  terminal e nada mais: era preciso abrir o navegador e digitar o endereço na mão.
  Agora o Deck sobe sem console e **abre a própria janela**, em modo aplicativo —
  sem barra de endereço nem abas, com entrada própria na barra de tarefas.
- **Clicar duas vezes no atalho dava erro de porta ocupada.** A segunda execução
  agora apenas traz a janela de volta e encerra.
- A página do release passa a dizer **qual arquivo baixar**: havia dois `.exe` no
  Windows — o instalador e o binário que o atualizador consome — sem nada
  distinguindo um do outro.

## [1.0.0-alpha.4] — 2026-09-03

### Adicionado

- **Ícone no executável do Windows**, com arte por tamanho: em 16 e 24 px só o
  raio, porque a marca inteira vira mancha nesse tamanho; de 32 px em diante o
  desenho completo. O instalador e a entrada em Programas e Recursos usam o mesmo
  ícone.
- **Propriedades do arquivo** preenchidas — nome, descrição, autor e licença
  aparecem na aba Detalhes do Windows.

## [1.0.0-alpha.3] — 2026-09-03

### Corrigido

- **A verificação de atualização não achava nada.** `/releases/latest` do GitHub
  ignora pré-lançamento e devolve 404 quando só existem alphas — inclusive com
  token válido. Agora a busca é na lista de releases e a escolha sai da comparação
  de versão, não da ordem da API. O download também: usava a URL
  `releases/latest/download/`, que tinha o mesmo defeito.
- Em repositório privado o binário passa a ser baixado pelo endpoint da API com
  `Accept: application/octet-stream`, porque o link público não aceita token.

## [1.0.0-alpha.2] — 2026-09-03

Primeira versão publicada de verdade. A `alpha.1` foi retirada: ela calculava o
consumo por janela deslizante, o que dava número errado.

### Corrigido

- **Janelas de limite estavam erradas.** Eram medidas como "as últimas 5h a partir
  de agora", mas o limite não desliza com o relógio: a janela começa na sua
  primeira mensagem e vale 5h a partir dali. A semana chegou a marcar 78% somando
  sete dias corridos para trás, atravessando ciclos já resetados; o ciclo real
  estava em 23%.

### Adicionado

- **Contador de reset** por janela — quanto falta para o ciclo de 5h e o de 7 dias
  renovarem, com o momento exato no tooltip. Janela vencida mostra zero em vez de
  arrastar consumo velho.
- **Identidade visual**: logo como marca na sidebar e favicon.
- **Nome de pasta escolhido por você**, em campo na própria sidebar.
- **Instalador para Windows** (Inno Setup): menu Iniciar, atalho opcional na área
  de trabalho, desinstalador e entrada em Programas e Recursos. Instala por
  usuário, sem pedir elevação.
- **Atualização dentro do programa**: o Deck consulta o GitHub Releases, avisa na
  barra quando há versão nova e instala com um clique. A troca **não reinicia** o
  Deck — reiniciar mataria os terminais abertos; a versão nova vale na próxima
  abertura. ⚠️ Enquanto o repositório for privado, isso exige um token em
  `SYNAPSE_DECK_GITHUB_TOKEN`.

### Limitação desta medição

O início do ciclo é **inferido do histórico local de mensagens**. O relógio real
do limite fica no servidor do provedor e não está nos arquivos — uso em outra
máquina pode divergir dessa âncora.

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
