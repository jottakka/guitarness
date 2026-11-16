# Guitarness: pesquisa de ensino, métricas, histórico e perfis

Data: 16/11/2026. Todas as leituras foram feitas nesta data. Este documento é pesquisa, não decisão: o [design do jogo](../design/design-do-jogo.md) continua mandando. Não repito o que já está na [pesquisa de game design](game-design.md) nem nas decisões iniciais de design; uso os nomes de habilidade e de veredicto do DESIGN §2 (`chord:G`, `change:G>C`, `Ok / Wrong(kind) / NotHeard`).

**Convenção.** `[E:Sn]` = evidência, fonte `Sn` da seção 9. `[R]` = resumo de buscador, não abri a página inteira: trate como "a fonte diz isso, não confirmei". `[I]` = inferência ou recomendação minha. `[N]` = não consegui verificar.

**Limite honesto do método.** Páginas que abri de fato: S1, S2, S3, S3b, S4, S5b, S7 (lista na seção 9). O resto veio de resumos de buscador. Duas leituras falharam: a tese da UPF sobre avaliação de performance de violão (acesso negado) e o artigo de Bicknell sobre Birdbrain (404). Nada aqui foi rodado: nenhum modelo foi baixado, nenhum código foi executado.

---

## 1. Resumo executivo

1. **Não existe modelo pronto e treinado de "ensino de violão" que valha usar.** Há conjuntos e protótipos acadêmicos para piano e canto, e um protótipo de avaliação de exercícios de violão (2020). Os poucos conjuntos de violão têm licença não comercial [R]. A avaliação de performance já está no juiz em Rust; o que falta é o **modelo do aluno**, e esse é pequeno, estatístico e se treina com o histórico do próprio jogador [I].
2. **Não achei nenhuma evidência de que um tutor LLM melhore a prática de um instrumento.** O único ensaio com sorteio que achei é de teoria musical, com 28 pessoas, e é trabalho de workshop [R:S12]. O estudo com violino é quase-experimental [R:S13]. Portanto o Maestro é uma hipótese, não um fato; deve ser medido dentro do jogo, e o diretor por regras precisa bastar sozinho [I].
3. **Primeiro modelo a construir: um placar de domínio por habilidade com Beta e esquecimento, mais agenda de revisão por regra simples.** FSRS entra em modo sombra, não como motor, porque foi feito e comparado para cartões de memória, e eu não achei validação para habilidade motora [S2, S3, N].
4. **Grave tudo, mostre pouco.** O banco guarda cada evento com veredicto, erro de tempo com sinal e qualidade do sinal. A tela mostra só o que sobe: totais, melhores marcas, "primeira vez". Nunca mostrar: percentual de acerto bruto, contagem de erros, ms por golpe, ranking de pontos fracos, comparação com outros.
5. **`NotHeard` não é erro e não entra no modelo.** Sem essa regra, cada falha do detector vira "o aluno é ruim nisso", que é a punição que o projeto quer evitar [I, coerente com DESIGN §1.8].
6. **Não pergunte "você tem TDAH ou ansiedade?".** Pergunte preferências ("sessões curtas", "sem números", "sem cronômetro"). Isso dá o mesmo benefício sem guardar dado de saúde, que na LGPD é dado sensível com consentimento específico [R:S20].
7. **Perfis:** um diretório por jogador, mais um "Convidado" que não guarda nada; perfil de violão e de dispositivo/calibração separados do perfil de jogador, porque o violão e o microfone são da casa, não da pessoa.
8. **Registre desde já as propensões do menu de 3 cartões** (quais foram oferecidas, com que probabilidade, qual foi escolhida). Custa quase nada e deixa trocar a regra por um bandit mais tarde sem perder os dados [I].

---

## 2. Pergunta 1: modelos e sistemas já treinados para APRENDIZAGEM de música

### 2.1 Avaliação de performance (assessment)

| Sistema | O que faz | Serve para a Guitarness? | Licença e execução local |
|---|---|---|---|
| Eremenko, Morsi, Narang, Serra (2020) | Sistema completo de avaliação de exercícios de violão; os autores dizem que as ferramentas de análise de som em apps de prática ainda não são confiáveis o bastante para o ensino profissional [R:S9] | Referência de método e de métricas, não de modelo | Não verifiquei licença nem código [N] |
| Dissertação da UPF "Automatic guitar performance assessment: datasets, algorithms and metrics" | Conjuntos, algoritmos e métricas de violão | Provável leitura útil de métricas; **não consegui abrir** (acesso negado) [N] | [N] |
| Conjunto "Five Guitar" (90 gravações) | Avaliação de violão | Só para pesquisa: licença CC BY-NC-SA 4.0 segundo o buscador; não comercial [R:S8] | Não comercial; não embarcar |
| YCU-PPE-III (CNN com atenção, >2.000 gravações de piano iniciante), Piano Skills Assessment, NeuroPiano (104 gravações, 2.255 comentários) | Notas de nível de pianistas | Piano, não violão; julgam "qualidade" de forma holística, o que é pouco útil para o nosso jogador | Licenças não confirmadas [R:S8, N] |
| SingPAD (canto) | 1.074 alunos, 2.587 notas, 1.080.852 respostas de nota, com acerto de altura e de ritmo por nota calculado por análise de áudio [S3b] | **É o conjunto mais parecido com o nosso problema**: um acerto/erro por evento, por habilidade, ao longo do tempo. Serve para estudar modelos de aluno, não para treinar o juiz | Repositório público; o artigo não cita licença [S3b, N] |

**Leitura [I].** O nosso juiz já decide acerto por evento com sinal e dispositivo conhecidos. Um modelo "holístico" que dá nota 1 a 5 a uma gravação responde a outra pergunta (nível global) e, para um iniciante ansioso, é justamente a informação que não se quer exibir. Não vale integrar.

### 2.2 Modelos de aluno (knowledge tracing) para música e habilidade motora

- **SingPAD [S3b]:** modelos testados no mesmo conjunto: BKT AUC 0,687; DKT 0,833; DKVMN 0,824; SAKT 0,840; AKT 0,842; GIKT 0,848 (validação cruzada em 5 partes). **Leitura [I]:** os modelos profundos ganham com um milhão de respostas de 1.074 pessoas. Um jogador sozinho gera centenas, não milhões; o ganho não se transfere. Para um único jogador, o modelo simples é o certo, e a versão com mais dados só faz sentido se um dia houver dados agregados voluntários.
- **BKT no piano:** uma apresentação da Universidade de Bielefeld aplica HMM/BKT a modos de prática de piano, sem resultados numéricos no que li [R:S11, N].
- **Violão:** não achei nenhum estudo de knowledge tracing de violão [R, N].
- **Habilidade motora:** não achei modelo de esquecimento validado para violão. O que há de sólido está em [pesquisa de game design](game-design.md) §3 (espaçamento e intercalação); aqui acrescento só a teoria OPTIMAL (seção 4.4).

### 2.3 Tutores LLM para música: que evidência existe

- **Chat Melody (2025)**: agente "ensinável" (o aluno explica a análise musical para a IA). 28 participantes; o grupo do agente teve notas maiores e menos carga cognitiva. É teoria musical, não prática de instrumento, e é submissão a workshop do CHI [R:S12].
- **Ou, Nogueira & Qin (Frontiers in Psychology, 2025)**: prática de violino com apps assistidos por IA; autoeficácia e desempenho melhoraram no grupo experimental. É quase-experimental, e o resumo não diz se o app usa LLM [R:S13].
- **Protocolo JoVE** de plataforma de IA para pianistas, com ansiedade medida pelo STAI-S: é só método, sem resultados no que vi [R:S14].
- **Conclusão [I]:** não há prova de que um LLM melhore tocar violão. O que existe de apoio ao Maestro é indireto (feedback menos frequente e centrado em acertos, seção 4.4). Trate o Maestro como experimento com medição própria, e mantenha o texto baseado em fatos que o núcleo calculou (já é uma decisão de design).

### 2.4 Licenças e execução local de um modelo de linguagem para o Maestro

Fontes: guias de terceiros [R:S15]; **não li as licenças primárias**.
- Qwen3: Apache 2.0, uso comercial sem restrição segundo os guias. Verificar o arquivo `LICENSE` do ponto de verificação exato.
- Llama: licença comunitária, grátis abaixo de 700 milhões de usuários ativos por mês, exige "Built with Llama".
- Gemma 3: termos próprios da Google, uso comercial permitido, mas com política de uso proibido e aviso obrigatório na distribuição; guias dizem que o Gemma 4 passou a Apache 2.0, informação secundária que não confirmei.
- **Recomendação [I]:** como o Maestro roda entre músicas e escreve texto curto sobre números que o núcleo já calculou, um modelo pequeno local basta e a licença mais simples (Apache 2.0) evita trabalho jurídico. A escolha do modelo já está fora deste documento (política do projeto: GPU não é usada durante a rodada).

---

## 3. Pergunta 2: métricas de erro e acerto

Princípio [I]: **separar o que se mede do que se mostra.** O banco guarda tudo, porque o dado permite reprocessar quando o juiz melhora; a interface mostra um subconjunto que só sobe. Aqui os contratos do DESIGN §1.8 mandam: `NotHeard` não é acerto nem erro.

### 3.1 Por evento (gravar sempre)

| Campo | Observação |
|---|---|
| `skill_key`, `lesson_id`, `mode`, `tempo_pct` | habilidade e contexto |
| `verdict` (`ok / wrong / notheard`) e `kind` | `kind`: oitava, corda vizinha, casa ao lado, corda morta, `Missing`, `Muffled`, `Extra`, `WrongFret`, golpe extra |
| `dt_ms` **com sinal**, relativo ao relógio calibrado | só vale se a calibração de ritmo estiver boa; senão grave `null` e a razão (`ritmo não julgado`) |
| `conf` ou margem da LLR por corda | permite reinterpretar depois com limiar diferente |
| `quality_flags` | SNR baixo, saturou, Bluetooth, ruído de fundo; é isso que decide se o evento entra no modelo |
| `contested`, `conceded` | contestar concede o evento (DESIGN §1.8) |
| `attempt_idx` no laço e no trecho | para tempo até acertar |
| `judge_version`, `bench_version`, `calibration_id` | sem isso, não dá para comparar sessões de antes e depois de uma melhora do juiz |

### 3.2 Por habilidade (derivado, recomputável do log)

| Métrica | Definição prática [I] | Observação |
|---|---|---|
| Taxa de limpo | `ok / (ok + wrong)`, sem `notheard` | guardar com intervalo (posterior Beta); número bruto com poucas tentativas engana |
| Taxa de abstenção | `notheard / total` | **métrica do juiz e do ambiente, não do jogador.** Se subir, o jogo deve pedir recalibração, não "tocar melhor" |
| Viés e dispersão de tempo | mediana e IQR (ou MAD) de `dt_ms` com sinal; viés separado de σ | viés se corrige com calibração; σ é o que melhora com a prática. Já previsto no DESIGN (viés e σ de ritmo) |
| Tempo de troca de acorde | do último ataque do acorde A até o primeiro ataque `Ok` do acorde B, em ms | também "trocas `Ok` por minuto" (modo Um Minuto de Trocas). Como referência de ensino, o JustinGuitar usa ~60 trocas/min como meta e ~30 como marco inicial, mas isso é prática de professor, não estudo [R:S10] |
| Tempo até acertar | tentativas e segundos do primeiro `Wrong` até o primeiro `Ok` no mesmo trecho | mede recuperação, não falha |
| Distribuição dos erros por `kind` | contagem por tipo | alimenta o texto do Maestro e a escolha de exercício; **não** vai para a tela como lista de erros |
| Quando praticou | `last_practised`, nº de sessões distintas | entra no agendador |
| Melhores marcas | máximo móvel de cada métrica boa | é o único "número" que a tela pode mostrar como progresso |

### 3.3 Por sessão

Duração ativa (sem pausas), habilidades tocadas, habilidades novas, `end_reason` (o jogador escolheu "Por hoje" / fechou / esgotou o tempo alvo), contestações, quantas vezes pediu pausa, intervalo desde a sessão anterior. **Guardar o motivo do fim da sessão é barato e é o melhor indicador de desistência por frustração que se tem sem perguntar** [I].

### 3.4 Quais métricas predizem aprendizagem

Resposta curta: **não há validação para violão** [N]. O que a literatura permite dizer:

- **Desempenho durante a prática não é aprendizagem.** Aprendizagem é o que persiste e transfere depois de um intervalo [I; ideia padrão da pesquisa de aprendizagem motora, não li fonte primária nesta rodada, N]. Logo, a métrica que mais se aproxima de aprendizagem é **o resultado na primeira tentativa de uma habilidade depois de dias sem tocá-la**, não a taxa de acerto no meio de um laço. Registre esse "primeiro bloco após intervalo" separadamente (`after_gap`) [I].
- **Taxa de acerto na prática perto de 85%** é o ponto de ótimo teórico para aprendizes por gradiente em tarefas binárias [S5]. O próprio autor não afirma que vale para estudantes humanos em geral; para percepção humana é plausível. Use 80 a 90% como faixa de alvo do controlador de andamento e trate como hipótese a medir [I].
- **Atenção e motivação (OPTIMAL):** expectativas positivas, autonomia e foco externo se somam para melhorar a aprendizagem motora [R:S6]. Isso sustenta: dar escolha (3 cartões), falar do efeito sonoro ("a troca soou ligada") em vez de mecânica de dedo ("mexa o indicador"), e não exibir comparação social negativa.
- **Ansiedade:** em instrumentistas, o medo de avaliação negativa é componente central da ansiedade de performance, e a ruminação após a execução se liga à autoavaliação negativa [R:S16]. Um estudo de 2026 sugere que a exposição online aumenta a ansiedade de avaliação, e que autocompaixão enfraquece a ligação com a ansiedade de performance; o texto que recuperei estava parcialmente truncado [R:S16, N]. **Não achei estudo que teste se um placar visível aumenta a ansiedade em adultos aprendendo.** A recomendação de não mostrar é por prudência, não por prova [I].

### 3.5 O que é nocivo mostrar a um jogador ansioso [I]

Nunca mostrar na interface padrão:

1. Percentual de acerto bruto, contagem de erros, número de `Wrong` por sessão.
2. Erro em milissegundos por golpe (a seta deslocada já mostra a direção; o número vira nota).
3. Lista de "habilidades mais fracas" ou ranking de pior para melhor.
4. Qualquer número que possa cair: média móvel, "domínio" em porcentagem que decai, "sequência" que quebra. A Melodics zera a sequência de notas consecutivas num erro [R:S17]; isso é o oposto do que o projeto quer.
5. Abstenção como se fosse erro. `NotHeard` aparece como "não ouvi, toque de novo" e culpa o ambiente.
6. Comparação com outros jogadores ou com "média da sua faixa".

Pode mostrar: totais acumulados, melhores marcas pessoais, "primeira vez que" (marcos), tempo de prática acumulado, e o texto do Maestro baseado em fatos. Um painel de números completo pode existir **atrás de uma ação explícita do jogador** ("Ver meus números"), desligado por padrão, para quem quer (o jogador com TDAH e autismo diferentes entre si; autonomia é um dos três fatores do OPTIMAL).

**Armadilha de honestidade [I]:** "números só sobem" só é verdade para totais e máximos. Uma média ou um "domínio" nunca deve ser desenhado como barra que sobe e desce às escondidas; use "melhor marca" e "total", e deixe o posterior interno invisível. Rocksmith+ usa uma barra "potencial" desbotada e uma barra sólida "conquistada" e baseia o nível em taxa de acerto por habilidade [S7]: é um bom desenho para não mostrar perda, desde que a sólida não recue.

---

## 4. Pergunta 3: histórico e modelos de aluno

### 4.1 Os modelos, o que exigem e como se comportam em frio

| Modelo | O que estima | Dados que exige | Partida a frio | Explicável? | Veredito para a Guitarness |
|---|---|---|---|---|---|
| **Beta por habilidade** (contagem de acertos e erros com esquecimento) | probabilidade de limpo | só o log de eventos | prior por família de habilidade (autorado) | Muito: "7 limpos de 9" | **v1** [I] |
| **BKT** (quatro parâmetros: conhece desde o início, aprende por oportunidade, erra sabendo, acerta chutando) [S4] | prob. de a habilidade estar dominada | muitas respostas por habilidade para ajustar os 4 parâmetros | parâmetros fixados à mão | Boa: parâmetros têm sentido | v2 candidato. **Aqui os parâmetros "slip" e "guess" têm correspondente medido**: taxa de falso erro e de falso acerto do juiz, que a bancada sintética do DESIGN §3 já produz [I]. O BKT padrão não modela esquecimento [S4, N] |
| **FSRS** (difícil D, estabilidade S, recuperabilidade R; FSRS-7 tem 34 parâmetros, 11 só da curva de esquecimento) [S3] | quando a recordação cai a 90% | histórico de revisões com nota por revisão; otimizador precisa de muitos cartões [S2] | parâmetros padrão (34 valores) existem; o quão bons são sem otimização, a página não diz [S3, N] | Média: S e R têm leitura simples | Sombra em v2. Licença do código de referência Rust: BSD-3-Clause, com agendador e otimizador e ligações para C, Python, Node, Dart [S2]. Combina com núcleo Rust |
| **Elo / Glicko** | habilidade do jogador e dificuldade do item como duelo | dificuldade de item inicial; funciona on-line | dificuldades autoradas | Boa | Pelánek: Elo é simples, robusto e eficaz em sistemas adaptativos [R:S18]. Glicko acrescenta incerteza [R:S18]. v2 para ordenar exercícios por dificuldade; **num PC com um jogador a vantagem é pequena** porque não há população |
| **IRT** | habilidade e dificuldade em escala comum | muitos alunos por item | exige população | Média | Fora de v1 e v2. Só se algum dia houver dados agregados voluntários [I] |
| **Birdbrain (Duolingo)** | probabilidade de acerto de exercício por aluno e dificuldade por item, atualizadas a cada tentativa | grande população | população | Baixa | Referência de produto. Dados de eficácia são da própria empresa, sem tamanho de efeito [S5b] |
| **Bandit** (Thompson sampling) para escolher exercício | qual cartão/exercício rende mais progresso | recompensa definida (progresso de aprendizagem) | prior forte, exploração baixa | Média | v3. Cuidado: o piloto do ASSISTments teve resultado menos positivo que o esperado, e a Thompson simples foi só um pouco melhor que sorteio [R:S19]. Há um estudo de 2026 em matemática com Thompson contextual, recompensa = mudança de habilidade estimada [R:S19] |

### 4.2 O primeiro modelo a construir

**"Domínio Beta com esquecimento + agenda por regra" (`skill_state`).** [I]

Para cada `skill_key` do jogador:

1. Estado: `(α, β)` de um Beta, `last_practised`, `best`, `due_at`.
2. Prior por família de habilidade e nível: por exemplo `Beta(2,2)` para família `chord`, ajustável. É uma escolha de partida a calibrar com a bancada.
3. Atualização por evento julgado (`ok` soma ao α, `wrong` ao β), **com peso menor** se `quality_flags` indica sinal ruim e **peso zero** para `notheard`. Contestado e concedido conta como `ok`.
4. Esquecimento: antes de cada atualização, encolher `(α, β)` rumo ao prior por `exp(-Δt/τ)`, com `τ` inicial de ~14 dias. É um chute de partida, não vem de fonte [I]. Medir e ajustar.
5. Agenda: habilidade "devida" quando `due_at` passou. O intervalo cresce em degraus (1, 2, 4, 8, 16 dias) a cada bloco bom em dias distintos e volta um degrau, nunca ao zero, quando o bloco após intervalo vai mal. Isso é um Leitner simples e explicável [I].
6. Escolha do cartão pelo diretor: do menu de 3, uma devida, uma nova, uma de preferência do jogador. A razão ("faz 5 dias que você não troca G>C; antes eram 7 limpos de 9") é sempre guardada junto.
7. Controlador de andamento: alvo 80 a 90% de limpo entre eventos ouvidos; dois laços limpos sobem 5% e três `Wrong` seguidos oferecem −10% em chip, que o jogador aceita ou ignora (já no DESIGN §2).

**Por que não começar por FSRS:** foi desenhado e comparado em dados de cartões do Anki [S2, S3]; mapear "bloco de prática" para "nota de revisão" é uma decisão nossa sem validação em habilidade motora [I, N]. **Por que não BKT em v1:** com poucas respostas por habilidade, os quatro parâmetros não se ajustam, e o resultado seria mais frágil que simplesmente contar [I].

**Como validar sem usuários em massa [I]:** repetir o histórico gravado (as fitas já registram o que aconteceu) e medir, por habilidade, se o modelo prevê a taxa de limpo do **próximo** bloco melhor do que a linha de base "repetir o último valor" (log-loss ou Brier). Só então ligar FSRS em sombra (calcular, gravar, não decidir) e comparar as duas previsões sobre as mesmas sessões. Trocar o motor só se vencer por margem combinada de antemão.

**Partida a frio [I]:** a "Passagem de som" e as primeiras sessões não alimentam o domínio. Primeiro dia: o diretor usa a escada do nível inicial autorada (Aprendiz) e nenhum agendamento. Do segundo bloco em diante o Beta começa a influenciar; antes de `n` eventos julgados por habilidade (sugestão: 8), a habilidade é marcada "ainda sem leitura" e não entra em ranking nem em texto do Maestro.

### 4.3 Explicável e gentil

- **Toda recomendação cita no máximo 3 fatos** do log, em frase positiva ("já são 7 trocas limpas de G para C; hoje vale uma volta rápida"). O Maestro nunca inventa número: recebe o JSON de fatos e só redige (decisão de design já tomada).
- **Botão "por que esta?"** mostra os mesmos fatos. O jogador pode dizer "não quero esta" e o diretor registra a recusa como preferência, não como falha.
- **Nada de decaimento visível.** Um domínio que esfria vira "pronta para uma volta", não "esquecida".
- **Reversível:** o jogador pode zerar o histórico de uma habilidade, ou tudo, a qualquer momento, e baixar os dados.
- **Nunca usar o modelo para negar acesso** a um modo ou a uma música; ele só ordena cartões (a lição do Rocksmith, em [pesquisa de game design](game-design.md) §2.1: desbloqueio é fricção para este jogador).

### 4.4 Como os apps comerciais adaptam a dificuldade (só o que está documentado)

| App | Documentado | Não documentado ou não verificado | Fonte |
|---|---|---|---|
| **Rocksmith+** | Começa com arranjos simplificados e acrescenta dedilhado mais difícil conforme se avança em cada trecho [R:S20]; segundo Jay Cohen, desacelera se não vê progresso e acelera quando há [R:S20]. Perfil de habilidades: nível = taxa de acerto por habilidade; barra desbotada = potencial, sólida = conquistado; não dá EXP por "encontrar" a técnica [S7] | Se o perfil influencia o que o jogo recomenda: o texto oficial não confirma [S7]. A fórmula do "mastery" de Rocksmith 2014 só aparece em fórum de fãs [R:S21, N] | S7, S20, S21 |
| **Yousician** | Opção "ajuste automático de velocidade" que reduz ou aumenta o andamento conforme a execução; trilha de missões; desde 2021 tudo desbloqueado para pular [R:S22] | Como as estrelas e a precisão são calculadas; se a trilha se adapta ao desempenho [R:S22, N] | S22 |
| **Fender Play** | Perguntas na entrada (instrumento, nível, estilo) geram um caminho; progresso rastreado; "Feedback Mode" em tempo real [R:S23] | Se a dificuldade muda pelo desempenho [R:S23, N] | S23 |
| **Simply Guitar** | Reconhece o que se toca e dá dicas em tempo real; modo PLAY, só no iOS, **não pontuado** "sem a pressão de ser avaliado" (central de ajuda oficial) [R:S24] | Como o caminho se adapta; relato de terceiros diz só que se escolhe nível e interesses no início [R:S24, N] | S24 |
| **Melodics** | Pontuação: precisão de tempo pesa mais que só cair no verde; estrelas por limiar; sequência de acertos zera no erro; todo segunda-feira, lições ligadas à fase atual [R:S17]. O blog da própria empresa manda **o jogador** baixar o andamento e subir quando melhorar [R:S17] | Adaptação automática de dificuldade: só uma agregadora de terceiros afirma, a documentação oficial não [R:S17, N] | S17 |
| **Duolingo (Birdbrain)** | Modelo que estima, por exercício, se o aluno acertará, e a dificuldade de cada tipo de exercício; o gerador de lições escolhe exercícios no nível certo; testes A/B da empresa dizem que aprenderam mais e voltaram mais [S5b, dados da empresa, sem tamanho de efeito] | Matemática do modelo e atualização (a postagem não explica) [S5b, N] | S5b |

**Síntese [I]:** o que está comprovadamente em produção em jogos de violão é **ajuste de andamento ou densidade por trecho**, com o jogador no controle; o que parece adaptação "inteligente" de currículo quase sempre não está documentado. A Guitarness fica competitiva fazendo bem o simples e **mostrando o motivo**, que é o que o Rocksmith foi criticado por não fazer ([pesquisa de game design](game-design.md) §2.1).

---

## 5. Pergunta 4: perfis

### 5.1 Perfil de jogador

| Campo | Padrão | Obs. |
|---|---|---|
| `id` local aleatório; `nickname` (opcional); `avatar` (variantes de Guitarnessy) | pergunta só o apelido | nunca pedir nome real, e-mail ou data de nascimento |
| Metas (múltipla escolha: "tocar uma música", "acompanhar canto", "ficar mais firme no ritmo", "só me divertir") | vazio | alimenta o diretor, que oferece cartões coerentes. Não texto livre |
| Nível autodeclarado (Aprendiz, Banda…) | Aprendiz, como no DESIGN §3 item 7 | o domínio medido assume depois |
| Mão dominante / canhoto | destro | espelha diagramas de braço; não muda o juiz de som |
| Preferência de palhetada (palheta / dedos) | palheta | muda a calibração de `pluck_s` e o juiz de golpe |
| **Preferências de sessão** (em lugar de "tenho TDAH/ansiedade"): duração alvo (5/10/15 min), sem cronômetro, números escondidos, animação reduzida, sem som de erro, "calma" | 10 min, sem cronômetro, números escondidos, uma novidade por sessão | é o pacote que as pesquisas do projeto já recomendam [[pesquisa de game design](game-design.md) §4] sem rotular a pessoa |
| Acessibilidade: daltonismo, tamanho de texto, movimento reduzido, entrada alternativa para contestar (pedal, tecla, voz), contraste | sistema operacional como ponto de partida | erro já nunca usa matiz (DESIGN §2) |
| Maestro: `off / local / nuvem` | **off** | com explicação do que sai da máquina (seção 5.4) |
| Idioma | inglês/português conforme sistema | |

### 5.2 Perfis de violão e de dispositivo/calibração

Pertencem ao **computador e ao instrumento**, não à pessoa [I]:

- `guitar`: apelido, tipo (náilon, aço, elétrico), nº de cordas, afinação (padrão, ou lista de cents de deslocamento por corda), capotraste, observações. Náilon contra aço muda inarmonicidade e decaimento (DESIGN §1.6/§1.7); por isso a calibração é por violão.
- `device_profile`: chave do dispositivo de entrada + taxa de amostragem, latência medida, MAD da latência, piso de ruído, ganho, interface (USB, Bluetooth, microfone interno), data e qualidade. Bluetooth sem calibração com MAD ≤ 15 ms deixa o ritmo "não julgado" (DESIGN §1.5).
- `calibration`: parâmetros da Passagem de som (`f0`, `B_s`, `env_s`, `pluck_s` por corda, viés do detector), com `schema_version`, ligados a um `guitar` e a um `device_profile`.
- **Invalidar** ao mudar o dispositivo de entrada, a taxa de amostragem, trocar de cordas (campo "troquei de cordas" manual) ou quando a autossuspeita do juiz (DESIGN §1.8 item 4) disparar. Nunca recalibrar sem avisar; oferecer o Raio-X de 20 s que o DESIGN já prevê.
- Nunca reaproveitar a calibração de um violão em outro silenciosamente.

### 5.3 Perfis de estilo de prática (derivados, não perguntados)

Calculados do log e **mostrados como "o que o jogo notou", editáveis** [I]:

- duração típica da sessão e hora do dia;
- preferência por música versus exercício (proporção do tempo);
- tolerância a andamento (andamento em que costuma aceitar subir);
- como escolhe entre os 3 cartões (sempre a primeira? sempre a nova?);
- onde costuma parar (mapeado por `end_reason` e por posição na sessão).

Usos: ajustar a duração sugerida, a ordem do menu e o tom do texto. **Nunca** para inferir diagnóstico e nunca exibido como rótulo ("você é impulsivo"). Se o jogador corrige ("na verdade gosto de sessões longas"), a preferência declarada vence a observada.

### 5.4 Privacidade e padrões [I, não é parecer jurídico]

- **Local primeiro.** Tudo em arquivos na máquina do jogador. Sem telemetria por padrão. Sem conta.
- **Sem dado de saúde.** Perguntar preferências em vez de condições: a LGPD classifica dados de saúde como sensíveis (art. 5º, II), exige consentimento específico e destacado para finalidades definidas (art. 11), e a base de legítimo interesse não vale para dado sensível [R:S20]. Um blog afirma que o art. 11 §1 alcança também dado que revela dado sensível por inferência; **confirmar no texto da lei** [R:S20, N]. A regra da GDPR (art. 9) conheço de memória e não verifiquei nesta rodada [N].
- **Áudio bruto:** não guardar por padrão. A contestação salva ±1 s de áudio como caso de regressão do DSP (DESIGN §1.8 item 5): isso é gravação da casa do jogador. Guardar só localmente, com aviso claro, apagável, e **nunca enviar** sem pedido explícito a cada vez.
- **Maestro em nuvem:** o prompt leva só agregados (contagens por habilidade, o que mudou desde a última sessão, preferências de sessão), nunca apelido, áudio nem metadados da máquina. Sem Maestro, o diretor funciona igual.
- **Exportar e apagar** por perfil, em um clique. Exportar em JSON legível.
- **Vários jogadores no mesmo PC:** um diretório e uma seleção de avatar por jogador, sem senha (o risco é constrangimento entre colegas de casa, não segurança); perfil **Convidado** que não grava histórico; nenhum placar entre perfis; nenhuma tela que mostre os números de um perfil dentro de outro; violão e dispositivo compartilhados. Se um perfil é apagado, os perfis de violão e dispositivo ficam.

---

## 6. Modelo de dados mínimo (proposta)

SQLite por perfil (`rusqlite`), com **log de eventos só de acréscimo** e tabelas derivadas recomputáveis. Isso permite reprocessar tudo quando o juiz melhorar (`judge_version`). As fitas de sessão existentes são a fonte de verdade; o banco guarda o que serve a consultas rápidas e a referência para a fita [I].

```sql
-- casa/maquina (um arquivo global)
CREATE TABLE guitar(id TEXT PRIMARY KEY, nickname TEXT, kind TEXT, strings INT,
  tuning_json TEXT, capo INT, notes TEXT, created_at INT);
CREATE TABLE device_profile(id TEXT PRIMARY KEY, input_key TEXT, sample_rate INT,
  latency_ms REAL, latency_mad_ms REAL, noise_floor_db REAL, gain_db REAL,
  interface TEXT, created_at INT, valid_until INT, quality TEXT);
CREATE TABLE calibration(id TEXT PRIMARY KEY, guitar_id TEXT, device_id TEXT,
  params_blob BLOB, schema_version INT, created_at INT, quality TEXT);

-- por jogador (um arquivo por perfil)
CREATE TABLE player(id TEXT PRIMARY KEY, nickname TEXT, avatar TEXT, hand TEXT,
  pick_style TEXT, level TEXT, prefs_json TEXT,   -- sessao, a11y, maestro_mode
  created_at INT, schema_version INT);
CREATE TABLE player_goal(goal TEXT PRIMARY KEY);

CREATE TABLE session(id TEXT PRIMARY KEY, started_at INT, ended_at INT, active_ms INT,
  guitar_id TEXT, device_id TEXT, calibration_id TEXT, end_reason TEXT,
  app_version TEXT, judge_version TEXT, gap_since_prev_days REAL);
CREATE TABLE lesson_run(id TEXT PRIMARY KEY, session_id TEXT, lesson_id TEXT, mode TEXT,
  params_json TEXT, tempo_pct INT, started_at INT, ended_at INT, tape_ref TEXT);
CREATE TABLE event(id INTEGER PRIMARY KEY, run_id TEXT, t_ms INT, skill_key TEXT,
  expected_json TEXT, verdict TEXT, kind TEXT, dt_ms REAL, conf REAL,
  quality_flags INT, contested INT, conceded INT, attempt_idx INT, after_gap INT);
CREATE INDEX event_skill ON event(skill_key, t_ms);

-- derivadas (recomputaveis a partir de event)
CREATE TABLE skill_state(skill_key TEXT PRIMARY KEY, alpha REAL, beta REAL,
  n_ok INT, n_wrong INT, n_notheard INT, last_practised INT, due_at INT,
  interval_step INT, best_json TEXT, model_version TEXT, updated_at INT);
CREATE TABLE milestone(key TEXT PRIMARY KEY, achieved_at INT);   -- so cresce

-- decisoes e aconselhamento
CREATE TABLE offer_log(id INTEGER PRIMARY KEY, session_id TEXT, ts INT,
  offered_json TEXT, probs_json TEXT, chosen TEXT, source TEXT,  -- 'rules'|'maestro'
  reason_json TEXT, outcome_json TEXT);
CREATE TABLE maestro_note(id INTEGER PRIMARY KEY, session_id TEXT, text TEXT,
  model TEXT, facts_json TEXT, created_at INT);
```

Notas:
- `facts_json` em `maestro_note` guarda os fatos que o núcleo entregou ao modelo; assim se pode conferir que o texto não inventou número.
- `probs_json` em `offer_log` é o que permite avaliar um bandit depois sem refazer a coleta.
- `after_gap` marca o primeiro bloco de uma habilidade depois de ≥ 2 dias sem tocá-la (a medida mais próxima de aprendizagem, 3.4).
- Nada na tabela de jogador descreve saúde ou diagnóstico.

---

## 7. Métricas: registrar versus mostrar

| Item | Registrar | Mostrar por padrão | Mostrar sob pedido |
|---|---|---|---|
| Veredicto e `kind` por evento | sim | só a forma na linha de acerto (DESIGN §2) | não |
| `dt_ms` com sinal | sim | só a posição da seta | gráfico do viés no tempo, por pedido |
| Taxa de limpo por habilidade | sim | não | sim, como "melhor bloco" e total |
| `NotHeard` / abstenção | sim | "não ouvi" + medidor de nível | sim, em "diagnóstico do som" |
| Tempo de troca e trocas/min | sim | **melhor marca** (só sobe) | histórico |
| Tempo até acertar | sim | não | não (uso interno do diretor) |
| Total de dias tocados, minutos, trocas boas | sim | sim (só cresce) | sim |
| `end_reason`, duração, intervalo entre sessões | sim | não | "ritmo de prática" |
| Domínio Beta (posterior) | sim | **nunca em número** | desenho "acende a casa" (a Vila), só sobe |
| Nota "ruim" do Maestro / listas de fraquezas | não existe | não | não |

---

## 8. Recomendações concretas, em ordem

1. **Agora (M-atual):** definir o esquema da seção 6, gravar o log de eventos e as ofertas dos cartões (`offer_log` com probabilidades), mesmo sem modelo. O dado de hoje não se recupera depois.
2. **M10 (modos):** implementar `skill_state` com Beta e esquecimento, agenda de degraus e o controlador de andamento 80–90%. Teste de aceitação: no histórico gravado, o modelo prevê o limpo do próximo bloco melhor que "repetir o último" (log-loss).
3. **Mesmo marco:** perfis de jogador e Convidado; `guitar`/`device_profile`/`calibration` ligados; preferências de sessão no lugar de rótulos de saúde; exportar e apagar.
4. **Depois:** FSRS em sombra (crate Rust, BSD-3) com mapeamento de bloco para nota documentado; BKT com `slip`/`guess` derivados da bancada do juiz; Elo/Glicko para dificuldade de exercício se houver exercícios o bastante.
5. **Maestro:** começar com texto sobre fatos do `skill_state`, modo `off` por padrão, e **medir** se seguir a sugestão leva a sessões seguintes mais longas ou com mais "volto amanhã" (n pequeno; só dá para descrever, não provar) [I].
6. **Bandit para o menu:** só depois de dezenas de sessões e só com recompensa de **progresso** (ganho em `after_gap`), não de tempo de tela.

---

## 9. Fontes (todas lidas em 16/11/2026)

**Páginas abertas (WebFetch):**
- S1. https://github.com/open-spaced-repetition/fsrs4anki/wiki/ABC-of-FSRS : índice; informa que a página migrou; sem licença nem parâmetros (aberta).
- S2. https://github.com/open-spaced-repetition/fsrs-rs : BSD-3-Clause, Rust, agendador e otimizador, ligações; não diz mínimo de revisões (aberta).
- S3. https://github.com/open-spaced-repetition/awesome-fsrs/wiki/The-Algorithm : D, S, R; FSRS-7 com 34 parâmetros, 11 na curva de esquecimento; vetor padrão existe (aberta).
- S3b. SingPAD: https://educationaldatamining.org/EDM2024/proceedings/2024.EDM-short-papers.30 : 1.074 alunos, 1.080.852 respostas, tabela de AUC (aberta).
- S4. https://en.wikipedia.org/wiki/Bayesian_knowledge_tracing : quatro parâmetros, etapas de atualização (aberta; a página não discute esquecimento nem identificabilidade).
- S5b. https://blog.duolingo.com/learning-how-to-help-you-learn-introducing-birdbrain : estima acerto por exercício; resultados de A/B da empresa; sem matemática (aberta).
- S7. https://www.ubisoft.com/en-us/game/rocksmith/plus/news-updates/3S8q6DNIp3JdcjedymA6xp/rocksmith-dev-diary-late-march-2022-skill-profiles : diário de desenvolvimento do Skill Profile (aberto).

**Só resumos de buscador (não abri a página inteira):**
- S5. Wilson, Shenhav, Straccia, Cohen (2019), "The Eighty Five Percent Rule for optimal learning", Nature Communications 10:4646, https://pmc.ncbi.nlm.nih.gov/articles/PMC6831579 [R]. Ótimo de ~85% para aprendizes por gradiente em tarefa binária; extensão a humanos não estabelecida.
- S6. Wulf & Lewthwaite (2016), OPTIMAL theory, Psychonomic Bulletin & Review; revisão de 2021 em Current Directions in Psychological Science: https://oasis.library.unlv.edu/kns_fac_articles/123 [R].
- S8. Conjuntos de avaliação (UPF; Five Guitar; YCU-PPE-III; PSA; NeuroPiano), via https://repositori.upf.edu/items/4cd08707-b8e2-415e-ab05-c3e4c16476aa/full (acesso negado), https://sites.duke.edu/dkusmiip/files/2022/11/Audio-Based_Piano_Performance_Evaluation_for_Beginners_With_Convolutional_Neural_Network_and_Attention_Mechanism.pdf [R, licenças N].
- S9. Eremenko, Morsi, Narang, Serra (2020), CSEDU, DOI 10.5220/0009817006290640, https://www.scitepress.org/Papers/2020/98170 [R].
- S10. JustinGuitar, One Minute Changes: https://justinguitar.com/guitar-lessons/one-minute-changes-exercise-b1-110 [R]. É material de ensino, não estudo.
- S11. Bielefeld, HMM no piano: https://techfak.uni-bielefeld.de/~abarch/presentations/Monitoring_The_Learning_Progress_In_Piano_Playing_With_Hidden_Markov_Model.pdf [R, só apresentação].
- S12. Jin et al. (2025), arXiv 2504.00636: https://arxiv.org/abs/2504.00636 [R].
- S13. Ou, Nogueira & Qin (2025), Frontiers in Psychology, DOI 10.3389/fpsyg.2025.1675762 [R].
- S14. JoVE, protocolo de plataforma de IA para pianistas: https://www.jove.com/v/69106/artificial-intelligence-music-platform-for-piano-majors-improves [R].
- S15. Guias de licença de LLM de terceiros: https://www.aiwiki.ai/wiki/open_weight_license_comparison, https://wz-it.com/en/knowledge/ki/open-llm-licenses-commercial-use/ [R, **licenças primárias não lidas**].
- S16. Ansiedade de performance: https://public-pages-files-2025.frontiersin.org/journals/psychology/articles/10.3389/fpsyg.2026.1742379/xml (texto parcialmente truncado), https://revuemusicaleoicrm.org/rmo-vol8-n1/performance-adolescent-pianists/ [R].
- S17. Melodics, pontuação e progresso: https://support.melodics.com/en/articles/15092017-lesson-scoring, https://support.melodics.com/en/articles/13876438-your-guide-to-progress-in-melodics [R].
- S18. Pelánek (2016), "Applications of the Elo rating system in adaptive educational systems", Computers & Education, https://www.fi.muni.cz/~xpelanek/publications/CAE-elo.pdf; Pelánek et al. (2017), UMUAI [R].
- S19. De Kerpel, Thuy, Benoit (2026), arXiv 2602.04347, https://arxiv.org/abs/2602.04347; ASSISTments L@S 2022, https://par.nsf.gov/servlets/purl/10354692 [R].
- S20. Rocksmith+ (adaptativo): https://www.slantmagazine.com/games/rocksmith-review/, https://www.rollingstone.com/product-recommendations/electronics/rocksmith-plus-ubisoft-pc-review-1234595472/ [R, resenhas]. LGPD: https://confidata.com.br/blog/lgpd-comentada-09-dados-sensiveis-anonimizacao e https://www.aurum.com.br/blog/lgpd-comentada/art-11-a-13-lgpd/ [R, blogs de advogados, não o texto da lei].
- S21. Fórum de fãs sobre "mastery" no Rocksmith 2014: https://customsforge.com/topic/4154-stupid-question-about-mastery [R, N].
- S22. Yousician: https://support.yousician.com/hc/en-us/articles/206930639-How-the-guitar-learning-path-works, https://yousician.com/blog/whats-new-in-yousician-february-2021-update [R].
- S23. Fender Play: https://play-support.fender.com/hc/en-us/articles/43004686659483-What-is-a-Path, https://play-support.fender.com/hc/en-us/articles/44538454058267-How-does-Fender-Play-work [R].
- S24. Simply Guitar: https://guitar-help.hellosimply.com/en/articles/14588852-what-is-play-in-simply-guitar, https://apps.apple.com/us/app/id1476695335 [R].

## 10. O que não consegui verificar

- Dissertação da UPF sobre avaliação de violão (acesso negado) e o código/licença do sistema de Eremenko et al.
- Licenças do SingPAD, YCU-PPE-III, NeuroPiano e Piano Skills Assessment; licença do Five Guitar só por resumo.
- Qualquer estudo de knowledge tracing de violão, ou de FSRS em habilidade motora: não achei.
- Mínimo de revisões para otimizar o FSRS, e quão bom é o vetor padrão sem otimização.
- Como Yousician, Fender Play, Simply Guitar e Melodics adaptam a dificuldade além do que está acima; a fórmula de "mastery" do Rocksmith+.
- Licenças primárias do Qwen3, Gemma e Llama (só guias de terceiros).
- Efeito de placar visível sobre ansiedade em adultos aprendendo (nenhum estudo achado).
- Texto da LGPD e da GDPR (só secundárias, e a GDPR de memória).
- Se o estudo de violino do Frontiers usa LLM.
- Valores de partida propostos ao Beta (`τ` de ~14 dias, prior `Beta(2,2)`, alvo de 80–90%, intervalo em degraus) são chutes de partida, não vêm de fonte.
