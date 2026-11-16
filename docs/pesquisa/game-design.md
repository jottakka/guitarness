# Guitarness: pesquisa de game design (ideias que funcionaram, e como aplicar)

Data: 16/11/2026. Todos os acessos web foram feitos nesta data. Este documento é pesquisa, não decisão: o [design do jogo](../design/design-do-jogo.md) continua mandando.

**Convenção de marcação.** `[E:Sn]` = evidência, com a fonte `Sn` da seção 13. `[I]` = opinião ou inferência minha, sem fonte. `[E?:Sn]` = a fonte diz isso, mas só vi resumo ou trecho, ou ela é secundária; trate como não confirmado. Quando não consegui abrir uma fonte, digo na seção 13.

**Limite honesto do método.** A maior parte das buscas devolveu resumos de buscador, não o texto integral. Só abri de fato a página do blog da Duolingo (S5) e a página da Wikipédia sobre Rocksmith (S1). Os outros "E" repousam em resumos, resumos de artigos e matérias. Os números de retenção da Duolingo vêm de terceiros e não foram confirmados.

---

## 1. Resumo executivo (10 pontos)

1. **O ganho do Rocksmith que mais importa para nós é a dificuldade dinâmica por trecho sem reprovação**: a música não acaba por erro, a densidade de notas sobe ou desce por fase, e o jogo guarda o último nível tocado [E:S1]. O Riff Repeater (trecho, velocidade, densidade) é o laço de prática que os jogadores de fato usam [E?:S3]. A crítica recorrente é latência, ajuste automático opaco e desbloqueios [E:S1]. Guitarness deve ter o laço e o ajuste, mas **visíveis e reversíveis**.
2. **Quem ensina com escuta de microfone perde confiança quando o juiz erra.** As reclamações mais comuns do Simply Guitar são falso erro e falso acerto de acorde [E?:S23]. Isso confirma o desenho do DESIGN.md (veredicto `NaoOuvi`, contestar, "acusar exige mais evidência") [I].
3. **Sequência (streak) é a mecânica de retenção mais forte e a mais perigosa.** A Duolingo usa e chama de ferramenta eficaz de aprendizado [E:S5]; críticos a veem como motivação por medo de perder [E?:S6]. Para Guitarness, a alternativa é uma contagem que só cresce, com "reencontro" ao voltar [I, ver §5].
4. **Prática com espaçamento e intercalada ajuda mesmo, e cabe numa sessão curta.** Espaçar reforça a retenção, e o intervalo ideal cresce com o prazo [E:S10]. Intercalar bate agrupar em tarefas motoras, com ressalvas [E:S13]. Estrutura de sessão: 3 blocos curtos de habilidades diferentes, revisitados em dias seguintes [I].
5. **Feedback menos frequente e centrado nos acertos aprende melhor.** Reduzir a frequência de feedback melhora a retenção [E?:S11]. Dar feedback após tentativas boas, e dar escolha ao aluno, melhora o aprendizado [E?:S14, E?:S9]. Isso casa com "o erro aparece como informação, o acerto como celebração" e com o metrônomo que some [I].
6. **Para o metrônomo que some ("gap"), não achei estudo controlado.** A literatura de sincronização com estímulo fala de correção de fase e de período [E?:S12]; que treino com buracos melhora o tempo interno é hipótese razoável, não fato [I]. Tratar como experimento do jogo, com medição própria.
7. **Modo sem derrota é um padrão consolidado**, com bons exemplos: Zen Mode do Alto (sem placar, sem queda) [E?:S18] e o Assist Mode do Celeste, renomeado de "Cheat Mode" por soar julgador, e cuja marca no save foi removida por excluir [E?:S17]. Lição de linguagem: nada de rótulo de "fácil".
8. **Evidência para TDAH em jogos é positiva mas frágil**: ensaio com 80 crianças e feedback imediato, recompensas e níveis [E?:S19]; piloto sem controle [E?:S19]; recursos de jogo podem atrapalhar quem tem poucos sintomas [E?:S19]. Para adultos aprendendo violão, quase não há dados; o que fica é princípio (feedback imediato, trechos curtos, escolha) e teste com usuários reais [I].
9. **Presença de outra pessoa (body doubling) tem evidência fraca.** Levantamentos de empresa e relatos positivos, sem ensaio controlado [E?:S20]. Uma "banda de gatos" que toca junto é uma versão barata e sem pressão social [I].
10. **O risco ético central é copiar as partes sombrias.** Padrões sombrios de tempo, dinheiro e capital social estão catalogados [E?:S16]. A regra "nada tira nada" do Guitarness já elimina quase todos. Faltam regras para notificações, loja e lembretes [I, §10].

---

## 2. O que se aprende de cada referência

### 2.1 Rocksmith (e Rock Band, Guitar Hero)

- **Faz** [E:S1]: notas como retângulos coloridos em pistas; falhas não encerram a música, mas se errar demais o jogo pausa para reposicionar a mão; dificuldade dinâmica por fase; Riff Repeater em todas as músicas; Guitarcade (minijogos de técnica) que desbloqueia após Desafios de Técnica; recepção boa, com críticas à latência, à dificuldade automática e ao sistema de desbloqueio.
- **Session Mode**: o time queria um espaço de improviso depois do primeiro jogo; a banda de IA tenta soar humana [E?:S2].
- **Minijogos de técnica "sem intimidação"**: a pré-estreia os descreve assim, cada um focado numa habilidade [E?:S2].
- **Eficácia**: não há ensaio controlado independente. Há um estudo de caso pequeno (4 pessoas, 60 dias): ensina algumas habilidades, é divertido, mas é difícil manter a motivação [E?:S4]. Os estudos "mais rápido para aprender" foram pagos pela Ubisoft e sem metodologia pública [E?:S4].
- **Rock Band 3 Pro**: lições em tempo real, tela mostra casas e cordas pressionadas, a Harmonix admite que mesmo o Pro Expert é uma abstração de tocar violão de verdade [E?:S24].
- **Leitura minha** [I]: o Rocksmith acertou no laço (trecho, velocidade, densidade) e no "não dá para perder"; errou ao esconder a regra do ajuste e ao prender conteúdo atrás de desbloqueio. Para o anxioso com TDAH, o desbloqueio é fricção, não recompensa.

### 2.2 Yousician, Simply Guitar, Fender Play

- **Yousician** escuta o microfone e rotula "perfeito" ou "cedo/tarde" [E?:S23]. Não achei estudo independente de eficácia [E?:S23]; as páginas de "ciência da aprendizagem" são marketing sem estudo citado.
- **Simply Guitar**: elogiado como envolvente e bom para começar, criticado por falta de profundidade [E?:S23] e por reconhecimento de acorde instável (Mi menor é o exemplo citado) [E?:S23].
- **Fender Play**: não pesquisei a fundo; sem evidência aqui.
- **Leitura minha** [I]: a confiança no juiz é o produto. Guitarness gasta o grosso do esforço técnico exatamente aqui; o design deve tornar a incerteza visível e o recurso de contestação barato.

### 2.3 Melodics e Synthesia

- **Melodics**: trilha guiada mais exploração livre, metas semanais com lições ajustadas ao estágio, etiquetas que levam de uma fraqueza a lições relacionadas, medidor de "Stardom" no placar [E?:S22]. Aceita microfone para bateria e pad [E?:S22].
- **Synthesia**: espera a nota certa antes de avançar; placar e progresso por peça [E?:S26]. Nenhum estudo isola "esperar" ou "pontuar" como variável [E?:S26]. Há um estudo que usa o Synthesia como linha de base contra um app de realidade mista: ambos ajudaram, o de realidade mista tocou melhor [E?:S26].
- **Leitura minha** [I]: o "modo espera" é o conforto de quem tem ansiedade; o modo "no tempo" é onde o tempo é treinado. O DESIGN.md já tem os dois (Ensaio e Show; Espera e No tempo).

### 2.4 Beat Saber, Rhythm Heaven, Taiko

- **Beat Saber**: não consegui o conteúdo da palestra do GDC (a busca devolveu só a lista da sessão) e não afirmo nada sobre a barra de energia ou o modo sem falha [não verificado].
- **Rhythm Heaven**: o criador Tsunku lista simplicidade, controle fácil e ritmo como o essencial; "não se apoiar nos personagens" [E?:S25]. A equipe tomou aulas de dança para sentir o ritmo [E?:S25]. Iwata comenta o apelo de ver alguém errar algo fácil e querer tentar [E?:S25].
- **Taiko**: sem fonte pesquisada. Marcar como **não verificado**.
- **Leitura minha** [I]: ritmo se sente no corpo antes de se ler. O modo de treino de tempo deve ter pouco texto e muito movimento, e ritmo e cor devem conversar com o corpo do jogador.

### 2.5 Duolingo e jogos casuais curtos

- A Duolingo descreve o streak como "ferramenta eficaz de aprendizado" e diz que a animação de marcos funciona como "power-up"; trocou a chama por uma fênix por ser mais universal; não publica números de retenção nesse post [E:S5].
- Números de terceiros (primeira semana é a mais frágil; congelamento cortou abandono em 21%; animação nova elevou a retenção de 7 dias em 1,7%) vêm de resumos de podcast e estudos de caso, **não confirmados** [E?:S6]. Não usar como argumento de projeto.
- A tensão ética: congelamento vendido com moeda do jogo mistura retenção e monetização [E?:S6]; um crítico acusa o streak de fazer mais trabalho de motivação do que a lição [E?:S6].
- **Zen Mode do Alto**: remove placar, moedas e quedas; nasceu de e-mails de jogadores que achavam o jogo relaxante [E?:S18]. [I] Referência certa para o "modo sem pressão" que o Guitarness deve oferecer como porta de entrada.
- **Padrão de sessão curta** de jogos casuais (sessões de 2 a 5 minutos, pausa sem perda): consenso de gênero, sem fonte aqui [I].

---

## 3. Evidência de aprendizagem e prática

| Tema | O que a fonte sustenta | Uso no Guitarness |
|---|---|---|
| Prática deliberada | Ericsson et al. 1993 ligaram prática solitária acumulada ao nível, mas a replicação duplo-cega de 2019 achou efeito menor, e a definição é disputada [E?:S27] | Não prometer "10 mil horas". Prática com meta e retorno vale, mas o tamanho do efeito é debatido [I] |
| Espaçamento | A revisão de 2006 (839 avaliações, 317 experimentos): o intervalo ótimo entre sessões cresce com o prazo desejado de retenção [E:S10] (tarefas verbais, não motoras) | Revisão espaçada de habilidades entre dias; transferência para motor é inferência [I] |
| Intercalação | Shea e Morgan (1979): prática aleatória retém mais que em bloco em teste com condições alteradas [E?:S13]; em música há pouco estudo e resultados mistos (clarinete) [E?:S13]; um estudo de hóquei achou resultados parecidos [E?:S13] | Misturar acorde, troca e batida na sessão; para iniciante absoluto, bloco curto primeiro e depois mistura [I] |
| Frequência de feedback | Reduzir a frequência de feedback tende a melhorar a retenção, ao custo do desempenho durante a prática (hipótese de orientação, Salmoni et al. 1984; Winstein e Schmidt 1990) [E?:S11] | Nível Palco e fase de retenção com menos retorno; "o jogo cala por 4 compassos" |
| Feedback após tentativas boas | Quem controla o feedback pede depois de tentativas que julga boas, e isso favorece o aprendizado [E?:S14]; mas o benefício some se o aluno nunca experimenta competência [E?:S14] | Destacar o que deu certo; mostrar o erro só quando o jogador pede "o que foi?" ou quando o padrão se repete [I] |
| Autonomia, expectativa, foco externo | A teoria OPTIMAL (2016) diz que expectativas positivas, autonomia e foco externo melhoram aprendizado [E?:S9]; há crítica de que ela explica mal a vantagem da prática autocontrolada [E?:S9] | Dar escolha de 1 entre 3 cartões; dar dicas sobre o som e o balanço ("faça a banda grudar"), não sobre o dedo [I] |
| Sincronização e tempo | A revisão de Repp e Su (2013) cobre correção de fase e de período; rastreamento fica menos preciso em andamentos lentos [E?:S12] | Janela de tempo proporcional ao andamento (já no DESIGN 1.5) |
| Metrônomo que some | **Não achei estudo controlado** de silêncio de clique ou "gap click" [E?:S28] | Tratar como hipótese a medir: ver §4 e o modo de Ritmo |
| Duração de sessão | **Não achei** dado sólido para TDAH em música; blogs de pianistas citam micropausas e passos pequenos [E?:S30] | Alvo de 10 a 20 min é opinião, com teste real [I] |

---

## 4. TDAH, ansiedade e "erro sem vergonha"

- **Recompensa e feedback imediatos**: um ensaio com 80 crianças com TDAH comparou versão gamificada (feedback imediato, recompensas, níveis) e controle por 8 semanas [E?:S19]; um piloto de 4 semanas sem grupo de controle também reportou melhora [E?:S19]. **Limites**: população infantil, resumos parciais.
- **Recursos de jogo podem atrapalhar**: em adultos, versões de jogo de tarefa de atenção ajudaram quem tinha mais sintomas e prejudicaram quem tinha menos, possivelmente por distração [E?:S19]. [I] Daí a regra: **menos efeito é mais**, e tudo deve poder ser desligado (§6).
- **Ansiedade de desempenho musical**: autocompaixão se correlaciona negativamente com ansiedade de apresentação em estudantes de música; ansiedade por avaliação online prevê mais ansiedade, e a autocompaixão amortece [E?:S21]. Um estudo pequeno (35 pessoas) achou mais redução de ansiedade com programa de autocompaixão do que com estratégias de desempenho [E?:S21]. **Sem dados em adultos iniciantes.**
- **Body doubling**: sem ensaios grandes; uma pesquisa de 117 adultos feita pelo próprio serviço relata mais foco e menos ansiedade, sem controle; outra com 1.600 estudantes achou ganho modesto e a razão principal era prestar contas [E?:S20]. Pode piorar para alguns [E?:S20].

**Traduções para o jogo** [I]:

1. **O erro é um fato sobre o som, nunca sobre a pessoa.** "Ouvi Am" em vez de "errou" (já no DESIGN §2).
2. **Resumo da rodada começa pelo que melhorou**, com uma frase verdadeira do núcleo (síntese inicial de design, não publicada, §4).
3. **Escolha pequena e frequente**: 1 entre 3 cartões, "Mais uma" e "Por hoje, tá ótimo" com o mesmo peso visual.
4. **Início sem fricção**: abrir o jogo já em uma frase tocável, sem menu; a primeira nota em menos de 10 segundos.
5. **Sem cronômetro de sessão visível.** O fim chega por convite do Maestro, não por contagem regressiva.
6. **Companhia sem avaliação**: a banda de gatos toca junto, mas nunca olha, comenta ou espera [inferência a partir de S20].
7. **Modo "Sem pressão"** (referência: S17, S18) com nome neutro e sem marca no save.

---

## 5. Juice sem poluição, e alternativas a combo

- **Juice**: "máximo de saída por mínimo de entrada"; efeitos pequenos de resposta tornam o jogo mais satisfatório [E?:S7]. Há uma palestra posterior defendendo moderação [E?:S7].
- **Regras para Guitarness** [I]:
  1. Cada efeito **carrega informação** (qual corda, quão cedo ou tarde) ou **não existe**. Brilho que não diz nada é ruído.
  2. **Um canal por vez**: o ataque mexe a seta (posição = adiantado/atrasado); o veredicto das cordas acende o painel; o som da banda responde por último. Nunca os três ao mesmo tempo no mesmo ponto da tela.
  3. **Acerto soa na hora, erro baixo e depois** (já no DESIGN §5).
  4. **Orçamento de efeitos**: no máximo N partículas por compasso; um interruptor de "reduzir efeitos" (acessibilidade, sensibilidade a luz e a movimento).
  5. **Sem cor para erro** (já no DESIGN §2): cinza e forma.
- **Alternativas a combo e streak que nunca tiram nada** [I]:

| Mecanismo | Como funciona | Por que não pune |
|---|---|---|
| **Luzes que só acendem** | Cada acerto ou dia tocado acende uma luz do letreiro; nunca apaga | Contagem monotônica |
| **Banda "encaixa"** | Mais instrumentos entram conforme o jogador acerta; ao errar, recuam para a bateria (DESIGN §5) | Informação, não perda |
| **Reencontro** | Voltar após dias gera um gesto de boas-vindas dos gatos, não uma pergunta | Inverte a lógica do streak |
| **Melhor trecho** | O jogo mostra a melhor sequência, não a atual | Só sobe |
| **Coleção** | Figurinhas de acordes e padrões | Aditiva |
| **Marcas de domínio** | "Achou a T sem dica": fatos, não pontos | Descreve, não avalia |
| **Pausa do combo** (já proposto) | Combo para, não zera | Sem queda |

---

## 6. Ideias agênticas e adaptativas

- **Dificuldade dinâmica**: Hunicke (2005) defende o ajuste dinâmico como resposta à tensão entre tédio e frustração [E?:S8; só a referência bibliográfica foi confirmada, não consegui ler o PDF]. A **versão do Rocksmith** (por fase, sem reprovar, guarda o último nível) é o modelo de uso [E:S1].
- **Regra de ouro** [I]: o ajuste automático **propõe, o jogador aceita** (chip visível, desfazer), e o motivo é dito em uma frase ("três compassos seguidos limpos; subir 5%?"). O DESIGN §3.1 já faz assim; manter.
- **O que o agente deve fazer** [I]:
  - traduzir um pedido em uma chamada de ferramenta da lista fechada (mais lento, mais fácil, repete);
  - explicar o último erro com fatos do núcleo;
  - propor o próximo passo no fim da rodada;
  - montar o gráfico de uma música (arranjador), sempre com "rascunho, confira".
- **O que o agente não deve fazer** [I]:
  - mudar limiar do juiz, ou decidir o que é "certo";
  - elogiar sem fato ("ótimo!") ou inventar número;
  - falar durante a rodada;
  - criar urgência, culpa ou lembrete emocional ("seus gatos sentem sua falta");
  - acumular memória de erros do jogador para uso fora do jogo.
- **Comando de voz durante o jogo** [I]: sem fonte específica de pesquisa. Princípios: apertar para falar (já no DESIGN §3.1); julgamento pausa enquanto fala; gramática fechada em português; confirmação visual em chip antes de valer; **o violão no microfone é o maior inimigo** (o jogador toca e fala ao mesmo tempo), então preferir pedal ou tecla a palavra de ativação.
- **Aprendizado do que pedir**: se o jogador sempre diz "mais devagar" no mesmo trecho, o jogo oferece um laço automático ali. O pedido vira dado de sinal, não de juízo.

---

## 7. Progressão e meta

- **Três necessidades** (autonomia, competência, relação) predizem prazer e jogar de novo [E?:S15]. Mapa para Guitarness [I]: autonomia = escolha de cartão, nível, fim da sessão; competência = marcas de domínio e músicas completas; relação = banda de gatos, e (opcional) troca assíncrona.
- **Biblioteca de músicas**: cada música importada ganha capa de disco com 3 níveis (síntese inicial de design, não publicada, §4). [I] A biblioteca é o "mundo" do jogador. Um bom primeiro conjunto são 5 a 8 músicas autorais curtas, escritas para ensinar um conceito por música (evita problema de direitos e deixa o conteúdo no tom certo).
- **Banda e avatar**: instrumentos que entram conforme o progresso [DESIGN §5]; o jogador escolhe o gato (cor, visor, fones). Cosméticos só **ganhos jogando** e **nunca vendidos por moeda de jogo** [I].
- **Desbloqueios**: o Rocksmith foi criticado pelo desbloqueio [E:S1]. [I] Regra: **tudo o que ensina está aberto desde o início**; desbloqueio é só cosmético, palco e capa.
- **Social opcional e assíncrono** [I]: "convite de show" (um amigo toca sua batida gravada), sem placar público e sem ranking. Qualquer compartilhamento é opt-in e vem sem número de desempenho.
- **Metas longas**: arco em estágios (Garagem → Beco → Telhado → Clube → Arena); metas semanais ajustáveis (Melodics tem metas semanais ajustadas ao estágio [E?:S22]); mas **meta é convite, nunca contagem a manter**.

---

## 8. Tabela ranqueada de ideias

Impacto e esforço: A/M/B (Alto, Médio, Baixo). Risco = risco para o jogador ansioso com TDAH.

| # | Ideia | Onde funcionou | Como aplica à Guitarness | Impacto | Esforço | Risco |
|---|---|---|---|---|---|---|
| 1 | Sem reprovação; falha só pausa e reposiciona | Rocksmith [E:S1]; Alto Zen [E?:S18] | Regra dura já existente; pausa gentil quando o jogador se perde | A | B | B |
| 2 | Laço de trecho com velocidade e densidade (Riff Repeater) | Rocksmith [E:S1] | Já no DESIGN (laço, andamento 40 a 120%); comando de voz "repete" | A | M | B |
| 3 | Dificuldade por trecho, proposta e reversível | Rocksmith [E:S1]; Hunicke [E?:S8] | Camadas Baixo/Batida/Dedilhado; Maestro propõe chip | A | M | M (se opaca) |
| 4 | Incerteza visível do juiz (`NaoOuvi`, contestar) | Crítica ao Simply Guitar [E?:S23] | Já no DESIGN; tratar como pilar de confiança | A | M | B |
| 5 | Sessão com 3 blocos intercalados, revisados em outros dias | Espaçamento [E:S10]; intercalação [E?:S13] | Estrutura da §9 abaixo; diretor por regras agenda revisão | A | M | B |
| 6 | Feedback centrado no acerto e em frequência reduzida | OPTIMAL e KR reduzido [E?:S9, E?:S11, E?:S14] | Resumo da rodada começa pelo que melhorou; Palco cala mais | A | B | B |
| 7 | Escolha de 1 entre 3 cartões e "Mais uma / Por hoje" | Autonomia [E?:S9, E?:S15] | Fim da rodada, botões de mesmo peso | A | B | B |
| 8 | Contagem que só cresce (luzes do letreiro) e "reencontro" | Contraste com streak [E:S5; E?:S6] | Dias tocados sem zerar; boas-vindas ao voltar | A | B | B |
| 9 | Modo "Sem pressão" (sem placar, sem avaliação de tempo) | Alto Zen [E?:S18]; Celeste [E?:S17] | Porta de entrada; nome neutro; sem marca no save | A | B | B |
| 10 | Banda que "encaixa" conforme o jogador acerta | Session Mode [E?:S2]; Rock Band [I] | Instrumentos entram; recuam para bateria ao errar (DESIGN §5) | A | M | B |
| 11 | Treino de ritmo com metrônomo que some (gap) | Hipótese; sem estudo controlado [E?:S28] | Modo novo; medir se o desvio melhora; opt-in | M | M | M (pode ser tenso) |
| 12 | Seta de tempo por posição (cedo/tarde sem número) | Yousician rotula cedo/tarde [E?:S23] | Já no DESIGN; sem números por padrão | A | B | B |
| 13 | Galeria de golpes como minijogos de técnica | Guitarcade [E:S1; E?:S2] | Cada golpe = habilidade; jogo curto de 60 a 90 s | M | M | B |
| 14 | Montador de músicas pelo nome | Sem equivalente direto [I] | Arranjador (rascunho, "confira"); acordes ficam locais | A | A | M (acordes errados) |
| 15 | Metas semanais ajustáveis | Melodics [E?:S22] | Convite no início, nunca contagem | M | B | M |
| 16 | Efeitos informativos e orçamento de efeitos | Juice [E?:S7] | Uma informação por canal; interruptor "reduzir efeitos" | M | M | B |
| 17 | Modo espera como padrão do Aprendiz | Synthesia [E?:S26] | Já em Ensaio e Espera | A | B | B |
| 18 | Voz com apertar para falar e chips | [I] | DESIGN §3.1 | M | M | M (ruído, violão) |
| 19 | Capas de disco por música e coleção de figurinhas | síntese inicial §4 [I] | Progresso aditivo | M | M | B |
| 20 | Social assíncrono opt-in | Relação [E?:S15]; body doubling [E?:S20] | "Convite de show", sem placar | B | A | M |
| 21 | Notificações de retorno | Duolingo [E?:S6] | **Não fazer** (ver §10) | B | B | A |
| 22 | Ranking e placar público | Gênero rítmico [I] | **Não fazer** | B | M | A |

---

## 9. Proposta: laço central, sessão de 10 a 20 min, primeira semana

### 9.1 Laço central [I]

1. **Convite** (≤ 10 s): o Maestro mostra 3 cartões (retomar, nova música, treino de ritmo). Sem menu.
2. **Contagem de um compasso**, com o clique do metrônomo acima de 6 kHz se não houver fone (DESIGN §5).
3. **Rodada de 40 a 90 s**: trecho curto, banda entra, o painel de cordas responde.
4. **Veredicto de rodada**: uma frase verdadeira de melhora, uma "observação" opcional, uma escolha: *Mais uma* / *Outro trecho* / *Por hoje, tá ótimo*.
5. **Fim**: uma luz acende; o jogo só sugere parar, nunca obriga.

### 9.2 Sessão de 10 a 20 minutos (alvo, sem teste real)

| Min | Bloco | Detalhe |
|---|---|---|
| 0–1 | Passagem de som rápida | Só se o ambiente mudou; afinação de cordas com neon |
| 1–4 | **Aquecimento** | Troca de acordes ou golpe abafado sobre a batida (Batida), modo espera |
| 4–10 | **Núcleo** | Trecho de uma música no andamento ajustado; até 3 rodadas, laço aceito |
| 10–14 | **Intercalado** | Outra habilidade: trecho de outro acorde, ou ritmo com metrônomo que some |
| 14–17 | **Palco** | Música inteira ou trecho "livre", banda toca com o jogador; sem avaliação |
| 17–20 | **Fecho** | Resumo, luz acesa, 1 cartão para amanhã; sessão pode acabar em qualquer minuto sem perda |

Regras: qualquer bloco pode ser pulado; a sessão **nunca depende de acabar**; o fim vem do jogador ou do Maestro, não de um cronômetro.

### 9.3 Primeira semana [I]

| Dia | Objetivo | Conteúdo |
|---|---|---|
| 1 | Primeira nota em segundos, confiança no juiz | Passagem de som curta; uma corda; um acorde fácil; Palco com a banda de bateria |
| 2 | Primeira troca | Dois acordes fáceis; modo espera; "Um Minuto de Trocas" como brincadeira, sem placar |
| 3 | Ritmo do corpo | Golpe abafado sobre batida; seta cedo/tarde; sem número |
| 4 | Primeira música curta | Música autoral, 3 acordes, camada Batida; laço aceito |
| 5 | Revisão espaçada | Acorde do dia 1 reaparece misturado; Maestro oferece "mais lento" se travar |
| 6 | Escolha livre | 3 cartões; jogador escolhe; palco com a banda de 2 instrumentos |
| 7 | Reencontro e marca | Primeira música inteira; capa de disco; sugestão de pedir música pelo nome |

Pular um dia só adia: o jogo retoma com "bom te ver", sem lembrar quantos dias passaram.

---

## 10. Anti-padrões (evitar, com motivo)

1. **Streak que zera** ou "congelamentos" comprados: transforma hábito em medo de perder; a própria literatura ética dos jogos cataloga o padrão de "jogar por compromisso de horário" [E?:S16]. Para quem tem ansiedade, é o contrário do que queremos.
2. **Vidas, tela de derrota, barra de energia, som áspero:** regra dura do projeto; também nega a expectativa positiva de competência [E?:S14].
3. **Juiz opaco com ajuste automático escondido** (crítica ao Rocksmith [E:S1]): o jogador perde o senso de causa.
4. **Reconhecimento instável que falsamente acusa** [E?:S23]: o falso erro é a punição real. Regra: acusar exige mais evidência do que confirmar (DESIGN §0).
5. **Efeitos em excesso** (tela cheia de brilho, partículas): piora para quem tem poucos sintomas [E?:S19] e para quem tem sensibilidade sensorial [I].
6. **Rótulo "fácil", marca no save, "jogo como pretendido"**: o Celeste teve de revisar o texto e remover a marca por excluir [E?:S17].
7. **Notificação emocional** ("seus gatos sentem sua falta"): culpa por design; vira padrão sombrio de capital social [E?:S16].
8. **Placar e ranking públicos:** a ansiedade por avaliação alheia amplia a ansiedade de desempenho [E?:S21].
9. **Desbloquear conteúdo de ensino:** fricção [E:S1].
10. **Loja que vende moeda** ou cosmético com prazo: monetização temporal e monetária [E?:S16].
11. **Prometer resultado**: "aprenda em N dias" sem estudo; os estudos de eficácia de apps de música são fracos ou pagos pelo fabricante [E?:S4, E?:S23].
12. **LLM no meio da rodada:** latência, erro e quebra de foco; o Maestro só fala entre rodadas (DESIGN §3).
13. **Pontuar o que o juiz não ouve:** o que o nível não julga fica "não julgado", visível (DESIGN §1.9).

---

## 11. Inspiração, não cópia

**Convenções genéricas do gênero (podem ser usadas livremente)** [I]:

- Pista de notas que corre até uma linha de acerto, e cartões de acorde com setas de golpe.
- Painel de seis cordas, casas numeradas, tablatura em linhas.
- Laço de trecho, velocidade ajustável, contagem de um compasso, modo espera.
- Dificuldade por camadas (só baixo, batida, dedilhado).
- Banda de acompanhamento que entra aos poucos; palco livre.
- Feedback cedo/tarde.
- Minijogos curtos de técnica.
- Lista de músicas com capas.

**Perto demais de uma identidade específica (evitar)** [I]:

| Referência | O que é identitário | O que fazer |
|---|---|---|
| Rocksmith | Faixas coloridas por corda com as cores exatas do jogo; retângulos que "giram" para acorde; Guitarcade com o formato tipo Galaga; nome "Riff Repeater", "Session Mode" | Usar o laço, mas com nome próprio ("Ensaio no beco"), a paleta de intervalos do Guitarness (`theory.js`) e minijogos com arte própria |
| Guitar Hero e Rock Band | Pista em perspectiva com os cinco botões coloridos; ícone de estrela; "Star Power", "Overdrive"; multidão que vaia | Evitar estrela e multidão que reage mal; o palco do Guitarness não vaia |
| Duolingo | Coruja, chama ou fênix de streak, "Duo", ligas e gemas, tom de ameaça bem-humorada | Nenhuma mascote que cobra; nenhuma liga; nenhum tom passivo-agressivo |
| Yousician | Escala de estrelas por trecho, notas rotuladas "Perfect" | Rótulos próprios, sem estrelas |
| Beat Saber | Sabres, blocos que cortam, estética neon em túnel | Não usar formato de corte nem túnel |
| Rhythm Heaven | Personagens e minijogos temáticos de estilo próprio | Referenciar a ideia (ritmo no corpo), não os cenários |
| Melodics | "Stardom", pads em grade de cores | Não usar medidor com nome parecido |
| Celeste | Rótulo "Assist Mode" | Usar nome neutro e próprio ("Sem pressão") |

Regras gerais [I]: **sem personagens, nomes de modo, trilhas ou paletas reconhecíveis de um jogo só**; nenhuma música comercial em arte ou tutorial; conferir a combinação de **cor + forma + animação** de cada elemento com uma pergunta simples: "um fã de X acharia que isto é de X?". Se sim, redesenhar. (Questão de direito autoral e marca não foi pesquisada; não é parecer jurídico.)

---

## 12. Perguntas em aberto para o usuário

1. **Quão firme é o "sem nenhuma perda"?** Por exemplo, a energia do público só sobe. Aceitamos que um trecho "reinicie" sozinho ao se perder, ou só pausa?
2. **Nível padrão de novos jogadores**: começar em Aprendiz com modo espera ligado, ou perguntar? (Impacta a primeira impressão e a confiança no juiz.)
3. **Metrônomo que some**: vale um experimento com 5 a 10 pessoas antes de investir no modo? Qual medida de sucesso (desvio-padrão do tempo, ou só a sensação)?
4. **Notificações**: nenhuma, ou um lembrete opcional que o jogador agenda e pode apagar sem comentário do jogo?
5. **Social**: queremos qualquer coisa além de salvar e mostrar a própria conquista? "Convite de show" justifica o esforço?
6. **Conteúdo base**: faremos 5 a 8 músicas autorais para ensinar, ou o plano é apoiar-se no Montador? (Impacta direitos e o primeiro mês.)
7. **Cosméticos**: só ganhos jogando, ou haverá loja um dia? Se houver, o que é inegociável?
8. **Dados**: aceitamos registrar localmente os erros do jogador para o diretor por regras, e mostrar de volta? (Privacidade e vergonha.)
9. **Voz**: gramática fechada em português basta, ou o Maestro precisa entender fala livre durante a rodada?
10. **Teste com usuários**: há acesso a 3 a 5 adultos com TDAH e ansiedade que toquem violão para sessões de teste? A maior parte da §4 é inferência e precisa disso.

---

## 13. Fontes (acesso em 16/11/2026)

Tipos: **A** = abri a página e li; **B** = vi só resumo ou trecho de busca; **C** = só referência bibliográfica, não li o conteúdo.

| ID | Fonte | URL | Tipo |
|---|---|---|---|
| S1 | Wikipédia, Rocksmith | https://en.wikipedia.org/wiki/Rocksmith | A |
| S2 | GamesBeat, pré-estreia Rocksmith 2014 (cita Bonardi e Higo) | https://gamesbeat.com/with-real-instruments-and-lessons-rocksmith-2014-seeks-to-banish-faux-guitar-video-games-preview/ | B |
| S3 | Gaming Trend, análise Rocksmith 2014; Engadget, prévia | https://gamingtrend.com/feature/reviews/can-always-get-better-rocksmith-2014-experienced-review ; https://www.engadget.com/2013/08/25/rocksmith-2014-amps-up-its-game-puts-a-jam-band-in-your-tv/ | B |
| S4 | Ly (2021), dissertação Univ. de Sydney; Graham e Schofield (2018); estudos pagos pela Ubisoft | https://ses.library.usyd.edu.au/handle/2123/26621 ; https://godisageek.com/2013/06/rocksmith-fastest-learn-guitar-research | B |
| S5 | Blog da Duolingo, animação de marcos do streak | https://blog.duolingo.com/streak-milestone-design-animation/ | A |
| S6 | Lenny's Newsletter (resumo de entrevista); Mobbin; estudos de caso | https://www.lennysnewsletter.com/p/duolingo-streaks ; https://mobbin.com/blog/streaks-are-about-losing-a-study-of-859-streak-designs | B (secundária) |
| S7 | Jonasson e Purho, "Juice it or Lose it" (GDC Vault) | https://gdcvault.com/play/1016487/Juice-It-or-Lose | B |
| S8 | Hunicke (2005), "The case for dynamic difficulty adjustment in games", ACE, DOI 10.1145/1178477.1178573 | https://users.cs.northwestern.edu/~hunicke/pubs/Hamlet.pdf (PDF ilegível pelo leitor) | C |
| S9 | Wulf e Lewthwaite (2016), OPTIMAL; crítica | https://link.springer.com/article/10.3758/s13423-015-0999-9 ; https://www.scapps.org/jems/index.php/1/article/view/1796 | B (Springer exigiu login; PDF da UNLV deu erro de conexão) |
| S10 | Cepeda et al. (2006), Psychological Bulletin | https://pubmed.ncbi.nlm.nih.gov/16719566/ | B |
| S11 | Salmoni et al. (1984); Winstein e Schmidt (1990), via resumos | https://www.frontiersin.org/journals/psychology/articles/10.3389/fpsyg.2020.00846/pdf | B |
| S12 | Repp e Su (2013), revisão | https://link.springer.com/article/10.3758/s13423-012-0371-2 | B |
| S13 | Shea e Morgan (1979); Carter e Grahn (2016); estudo de hóquei | https://Www.Gwern.net/doc/psychology/spaced-repetition/1979-shea.pdf ; https://www.frontiersin.org/journals/psychology/articles/10.3389/fpsyg.2016.01251/pdf ; https://www.jssm.org/jssm-11-304.xml-Fulltext | B (a atribuição de cada URL a cada estudo veio de lista de busca; confirmar) |
| S14 | Chiviacowsky, Wulf e Lewthwaite (2012); outros | https://pmc.ncbi.nlm.nih.gov/articles/PMC3487418 ; https://www.frontiersin.org/articles/10.3389/fpsyg.2012.00458/text | B |
| S15 | Ryan, Rigby e Przybylski (2006) | https://link.springer.com/content/pdf/10.1007%2Fs11031-006-9051-8.pdf | B |
| S16 | Zagal, Björk e Lewis (2013), padrões sombrios | https://deceptive.design/articles/dark-patterns-in-the-design-of-games ; https://my.eng.utah.edu/~zagal/Papers/Zagal_et_al_DarkPatterns.pdf | B |
| S17 | Celeste: Wireframe; Vice | https://wireframe.raspberrypi.com/articles/the-new-normal-the-indie-developers-changing-our-conceptions-of-difficulty ; https://www.vice.com/en/article/celeste-assist-mode-change-and-accessibility | B |
| S18 | Engadget, Zen Mode do Alto's Adventure | https://www.engadget.com/2016-06-01-altos-adventure-zen-mode.html | B |
| S19 | TDAH e gamificação: ensaio com 80 crianças (Frontiers in Education 2025); piloto StarRuckus (JMIR Hum Factors 2025); estudo adulto sobre recursos de jogo | https://www.frontiersin.org/articles/10.3389/feduc.2025.1668260 ; https://humanfactors.jmir.org/2025/1/e70479 ; https://pmc.ncbi.nlm.nih.gov/articles/PMC9946965 | B (a URL do estudo com N=94 é incerta; verificar) |
| S20 | Body doubling: PopSci; resumo de pesquisa com 117 adultos (serviço FLOWN) | https://www.popsci.com/health/body-doubling-focus-benefits/ ; https://neurodiversity.org/?p=11449 | B |
| S21 | Autocompaixão e ansiedade de desempenho musical | https://www.frontiersin.org/articles/10.3389/fpsyg.2026.1787708 ; https://ruor.uottawa.ca/items/3125a794-1599-4428-b2d3-bf50178916ef | B |
| S22 | Melodics, como funciona; suporte | https://melodics.com/how-it-works ; https://support.melodics.com/en/articles/13876438-your-guide-to-progress-in-melodics | B |
| S23 | Yousician e Simply Guitar: análises e avaliações de loja | https://violetrecording.com/yousician-review/ ; https://apps.apple.com/us/app/id1476695335 ; https://promusicianhub.com/simply-guitar-review | B |
| S24 | Rock Band 3 Pro: Engadget; Time | https://www.engadget.com/2010/09/14/rock-band-3-guitar-trainer-preview-fret-nice/ ; https://techland.time.com/2010/10/26/rock-band-3-review-you-might-accidentally-learn-an-instrument/print/ | B |
| S25 | Iwata Asks, Rhythm Heaven | https://iwataasks.nintendo.com/interviews/ds/rhythm-heaven/0/0 ; https://www.nintendoworldreport.com/news/18099/iwata-asks--rhythm-heaven | B |
| S26 | Synthesia (App Store); estudo de realidade mista com Synthesia como linha de base | https://apps.apple.com/us/app/synthesia/id579518445 ; https://link.springer.com/article/10.1007/s10055-024-00953-w | B |
| S27 | Ericsson et al. (1993) revisitado (replicação) | https://www.ncbi.nlm.nih.gov/pmc/articles/PMC6731745/ | B |
| S28 | Treino com clique que some: **nenhum estudo controlado encontrado** | (busca sem resultado) | n/a |
| S30 | Micropausas e TDAH em pianistas (blog; opinião especializada) | https://pianodao.com/2026/06/13/adhd-and-micro-breaks/ | B |

**Não verificados ou sem fonte** (declarados, não inventados): Beat Saber (conteúdo da palestra do GDC não acessado), Taiko, Fender Play, Guitar Hero (prática), UX de voz durante jogo, duração ideal de sessão para TDAH, e qualquer número de retenção da Duolingo. Para **S9**, **S8** e **S12**, só vi resumos; as afirmações são do resumo, não do texto integral.
