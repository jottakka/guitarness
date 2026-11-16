# Relatório da rodada 2

Data: 16/11/2026. Continua o [relatório da fatia Pulso](fatia-pulso.md). Regras de desenho: [adendo 02](../design/adendo-02.md). Visão de crates e fluxo: [visão geral da arquitetura](../arquitetura/visao-geral.md).

**Nada aqui vem de violão, microfone ou alto-falante reais.** Toda a evidência é sintética (violão simulado em `guitarness-sim`, jogador virtual, backend falso). Os portões de acorde continuam `Locked`.

## 1. O que foi construído

- **`guitarness-learn`** (novo): modelo do aluno. Evidência por rodada, domínio por habilidade, placar exibido, agenda de revisão, escolha do próximo bloco, treinador por regras, armazenamento em disco e uma interface de "sombra".
- **`guitarness-verify`** (novo): verificador de acordes. Conjuntos de confusão fechados, uma testemunha DSP ("pente"), a regra de decisão e uma bancada sintética.
- **`guitarness-core`**: contrato por evento (`contract`), que separa o que o evento pede do que o microfone consegue julgar. Os portões também passaram a recusar abertura por evidência sintética (seção 5).
- **`guitarness-cli`**: comando `guitarness verify`.
- **`guitarness-game`**: o aprendiz e o contrato ligados à tela do Pulso.

## 2. O contrato por evento

Cada evento do gráfico **pede** coisas ao jogador. O microfone só **julga** algumas. As duas listas são diferentes e a diferença é calculada na hora, a partir da tabela de portões e da calibração. Ela nunca é gravada no gráfico (`chart.json` segue em `v = 1`).

`guitarness_core::contract::contract(gráfico, posição, portões, timing_julgado)` devolve um `EventContract` com oito aspectos:

| Aspecto | Observação |
|---|---|
| `Attack`, `Timing` | Julgados quando o portão deixa e há calibração. Sem calibração, `Attack` vira guia. |
| `Direction` | Sempre guia: o juiz não julga direção. |
| `Accent`, `Muted` | Pedidos por estilo (acento, abafado). Guia. |
| `Chord`, `Strings`, `Fret` | Guia enquanto o portão de acorde estiver `Locked`. |

Cada aspecto pedido é `Check::Judged(estado do portão)` ou `Check::Guide(motivo)`. Um aspecto-guia **nunca recebe marca de resultado**. Posição inválida, camada inválida ou pausa dão contrato vazio.

No jogo (`contract.rs`), o aspecto julgado usa as marcas que já existiam. O aspecto-guia aparece como instrução em cinza: anel, seta de direção, cruz para abafado, losango para acento. Uma única função (`feedback::shown`) decide se a marca aparece. Instrução nunca sugere verificação.

## 3. Aprendiz (`guitarness-learn`)

### Evidência

`evidence_of_round` lê só o veredito final de cada evento esperado: vale o último, e uma contestação posterior o zera. Um golpe é um ataque ouvido e não contestado. Fantasmas e `Extra` não geram linha.

| Caso | Peso |
|---|---|
| Julgado (sucesso ou falha) | 1 |
| Provisório (portão `Provisional`) | 0,5 |
| Contestado, "não ouvi" ou "não julgado" | 0 |

Esses três últimos casos não contam como erro nem como acerto.

### Domínio e agenda

- Cada habilidade (`pulse:quarter`, `pulse:eighth-down`, `pulse:eighth-up`, `pulse:free`) tem uma estimativa Beta com priori (2, 2) e decaimento de 14 dias.
- Níveis: "bom" a partir de 0,80, "fraco" abaixo de 0,60, com pelo menos 8 observações efetivas.
- A agenda de revisão é **oculta**: intervalos de 1, 2, 4, 8 e 16 dias. O jogo não mostra datas de vencimento nem lista de itens atrasados.
- Reexecutar o log com uma contestação dá exatamente o mesmo resultado que reexecutá-lo sem o evento (testado em 200 logs).

### Placar exibido

`DisplayLedger` guarda só números que **nunca descem** (o tipo `Mono` só aceita `add` e `raise`): golpes, blocos, tempo ativo, dias tocados (uma contagem, nunca uma sequência), melhor andamento por exercício e marcos. Carregar um arquivo faz fusão pelo máximo campo a campo, então um arquivo antigo ou adulterado não reduz nada. Contestação não mexe no placar. Nota fraca não remove marca. Blocos de sondagem somam nos totais, mas não em marcas nem marcos.

### Escolha do próximo bloco

`picker` oferece **um** bloco por vez. Regras em vigor:

- No máximo uma revisão (a mais antiga vencida) e dois desafios. Duplicatas por especificação são descartadas.
- Toda mudança oferecida é para cima (andamento ou ritmo). Nunca há "desce" automático, nem cadeia de recuperação.
- Depois de uma **sondagem de retenção** vem um único bloco seguinte. Bloco fraco leva a uma recuperação de −10% no andamento, com piso de 20.
- "Outra coisa" (tecla `O` no jogo) sempre existe e dá a volta pela lista.
- O início é 60 bpm, semínimas, 4 compassos.

O `coach` monta 2 a 3 jogadas (as sete jogadas conhecidas, com "parar" sempre por último) e frases fixas, validadas por um verificador de texto. A escolha é por regras. Há um ponto de entrada para outro escolhedor, que nada chama hoje.

### Armazenamento

`learn.jsonl` é só de acréscimo, uma linha JSON por registro (`block`, `dispute`, `offer`, `choice`, `shadow`). Números de ponto flutuante vão também em bits, para voltar idênticos. Uma última linha incompleta é cortada na abertura; linhas desconhecidas ou ruins são contadas e ignoradas. O perfil de visitante usa memória e não toca o disco.

### Sombra

`Shadow` é só uma interface. O padrão, `NoShadow`, não observa nem prevê. Nada no seletor nem nos placares lê a sombra. **FSRS não está implementado**; só a interface existe, e a regra é que ele só poderia rodar como sombra.

## 4. Verificador de acordes (`guitarness-verify`)

### Conjuntos de confusão

`confusion_set(gráfico, posição, estrito)` devolve a hipótese esperada e até 24 alternativas, em ordem: nota faltando, traste errado (±1 e solta), corda a mais, acorde vizinho e tudo abafado. Duplicatas por som resultante são removidas. Não há conjunto para abafado, fantasma, pausa, nota solta ou acorde desconhecido. Fora do trecho da palhetada, a corda é "pode" (descendo, cordas 0 a 5; subindo, 2 a 5).

### Testemunha "pente" (`CombWitness`)

Corta a janela antes e depois do ataque, ajusta um dicionário de harmônicos às cordas esperadas e emite uma opinião: `Supports` (apoia o esperado), `Names` (nomeia uma alternativa) ou `Abstains` (com motivo, como pouco sinal ou ambiguidade). Ela se afina ao instrumento: aceita desafinação do violão inteiro e de cada corda dentro de uma faixa, e declara `Fault::Detuned` quando a afinação foge dela. Custo por evento: média de 0,53 ms, máximo de 2,70 ms. Sem alocação no caminho medido.

### Regra de decisão (`decide`)

Pura e recusa entradas malformadas. Nesta ordem:

1. Portão de identidade de acorde `Locked`: não julga.
2. Margem não finita: indisponível.
3. `Ok`: apoio acima da margem e nenhuma testemunha nomeando uma alternativa com força.
4. Apoio e veto juntos: conflito.
5. Acusação (`Wrong`): exige **duas testemunhas de famílias diferentes** que nomeiem exatamente a mesma alternativa, uma margem de acusação vinda de fitas, e o portão de acorde e o do tipo acusado em `Open`.

`ChordVerdict` não carrega a contagem de testemunhas nem o estado dos portões. Quem chama pesa o `Ok` (seção 6).

### Bancada sintética

`bench::run` varre uma grade de acordes, golpes, ruído, reverberação, microfones e acordes anteriores ainda soando, com verdade de referência. Há uma região de sementes reservada, que o desenvolvimento nunca usa. A bancada também confere que os quatro tipos de acorde continuam `Locked` na tabela proposta.

## 5. Portões de acorde ficam trancados

Evidência sintética **não abre** portão de acorde. `gates::synthetic_can_unlock` é verdadeiro só para `Attack`, `Timing` e `Tendency`. Para os outros sete tipos, `apply_evidence` e a leitura de arquivo rebaixam `Provisional` sem fita real para `Locked`, com o motivo registrado. Só fita real abre. A testemunha precisa de gravações reais (veja o [roteiro](../roteiro-primeira-sessao.md)) antes de qualquer julgamento de acorde.

## 6. Como a testemunha se comporta

Bloco de teste novo, nunca usado antes (n = 8160 por condição). "Falso nome" é a testemunha nomear uma alternativa errada quando o acorde estava certo. O critério do projeto é limite superior de Wilson (95%) de no máximo 0,5%.

| Condição | Falso nome (limite Wilson, %) | Apoia o acorde certo (%) | Passa |
|---|---|---|---|
| Afinação exata 440 Hz | 0,30 | 90,0 | sim |
| Violão inteiro −30 cents | 0,21 | 90,0 | sim |
| Violão inteiro −15 cents | 0,24 | 90,4 | sim |
| Violão inteiro +15 cents | 0,27 | 90,1 | sim |
| Violão inteiro +30 cents | 0,23 | 90,0 | sim |
| −30 cents com σ = 5 por corda | 0,42 | 73,5 | sim |
| +30 cents com σ = 5 por corda | 0,44 | 75,3 | sim |
| Por corda, σ = 5 cents | 0,60 | 86,0 | **não** |
| Por corda, σ = 10 cents | 2,97 | 62,0 | **não** |

Na versão anterior da testemunha, o violão a −15 cents gerava 773 falsos nomes em 8160 (limite 10,1%). A tolerância à desafinação resolveu isso para desafinação do instrumento inteiro. A desafinação **por corda** acima de cerca de 5 cents continua fora da faixa em que a testemunha é segura.

Outros números do mesmo bloco, em afinação exata:

- Erro nomeado corretamente: traste errado 84,4%, corda a mais 89,9%. Nota faltando só é nomeada quando visível.
- Em palhetadas com traste errado, a testemunha apoiou o acorde errado 116 vezes em 13 440 (0,9%). Antes eram 96 (0,7%). Com desafinação o número ficou entre 98 e 119. Ou seja, **cerca de 0,8% dos acordes errados seriam aprovados**, e esse número subiu um pouco com a tolerância. Os intervalos de confiança se sobrepõem.
- Desafinação plana de +80 cents ou mais é nomeada como traste errado (um semitom de diferença: ambiguidade inevitável).
- Quando o violão está fora da faixa, a testemunha responde `Detuned` em vez de opinar. Com ±30 cents e σ = 5, isso é 13 a 15% dos golpes.

Uma tentativa de endurecer o critério de dispersão por corda foi descartada: reduzia o reconhecimento de traste errado com σ = 5 de 82% para 38–67%, abaixo do mínimo aceito.

## 7. `guitarness verify`

```text
guitarness verify FITA... [--gates ARQUIVO] [--json] [--detalhes]
```

Só relata. Não grava tabela de portões e não muda veredito. Recorta cada `Strum` do gráfico que o juiz casou com um ataque, roda a testemunha e `decide`, e escreve uma linha por tomada e acorde. Códigos de saída: 0 feito, 2 linha de comando inválida, 20 fita ilegível. Fita sem golpes de acorde dá saída vazia e código 0.

Regras que o comando segue:

- Com o portão de acorde `Locked`, todo golpe é "guia". A leitura da testemunha é medição, não veredito.
- Uma testemunha nunca acusa. Um `Ok` só da DSP pesa no máximo 0,5 e só no nível do acorde, nunca por corda.
- `Fault::Detuned` não é certo nem errado. A partir de cerca de 3 por fita, o texto sugere `guitarness tune`.
- Dica de corda ou tipo só aparece quando o mesmo erro único (traste errado ou corda a mais) se repete em **dois ou mais golpes seguidos**, com ajuste de afinação confiável, e só em `--detalhes` ou `--json`. "Vizinho" nunca aparece como tipo.
- O texto padrão é curto, em português, sem números (exceto cifras).

## 8. No jogo

- **Aprendiz.** Ao fim de cada rodada, o jogo monta o registro do bloco e atualiza os dois livros (domínio e placar). O botão de tocar executa a oferta atual. Há um chip de andamento único, a tecla `O` pede outra coisa, e um bloco de sondagem não mostra resumo. Escritas vão para uma thread própria, fora do quadro. O perfil é de visitante: `Learner::open_local` existe e carrega um perfil gravado, mas o jogo ainda não o chama.
- **Contrato.** Seção 2. Acorde, corda e traste são guias. Não há marca de resultado de acorde na tela.
- **Texto ao jogador (regras fixadas, ainda sem tela de acorde).** Acusação só no nível do acorde ("acorde não confere"). Uma testemunha nunca acusa. Violão desafinado não recebe certo nem errado, e a sugestão do afinador vem depois de cerca de 3 golpes na sessão. Falha na checagem de afinação dá uma nova tentativa genérica, sem correção de corda ou traste.

## 9. Decisões em vigor

1. Portões de acorde só abrem com fita real. Evidência sintética nunca os abre.
2. Acusar só no nível do acorde. Dica de corda ou tipo só com repetição em 2 ou mais golpes seguidos. "Vizinho" nunca é mostrado.
3. Uma testemunha nunca acusa. `Ok` só da DSP pesa no máximo 0,5 e é só no nível do acorde. Elogio genérico, nunca por corda.
4. Desafinado não é certo nem errado, não penaliza, e sugere `guitarness tune`.
5. Evidência contestada, "não ouvi" e "não julgado" pesam zero; provisória pesa metade.
6. Só números que não descem aparecem na tela. Não há lista de pendências nem datas de revisão visíveis.
7. Depois de uma sondagem de retenção, vem um único bloco seguinte. Toda mudança oferecida é para cima.
8. FSRS só pode rodar em sombra, sem influenciar a oferta.
9. Escritas em disco nunca acontecem no quadro. O perfil de visitante não usa arquivo.

## 10. Limites abertos

- **Tudo sintético.** O violão simulado não tem inarmonicidade (sem parâmetro de dispersão), e isso não foi medido. Acordes reais podem se comportar de outro modo.
- **Desafinação por corda.** A partir de σ de cerca de 5 cents a testemunha falha o critério de falso nome. Pergunta em aberto: exigir uma passagem pelo afinador antes de qualquer julgamento de acorde.
- **Aprovações indevidas.** Cerca de 0,8% dos acordes errados recebem apoio, e uma corda ausente que dobra a oitava de outra é inobservável, mas ainda recebe apoio (82% na grade de desenvolvimento). Por isso um `Ok` só da DSP pesa no máximo 0,5.
- **Graves.** Não há prova de ausência abaixo de 150 Hz. A margem de apoio é estreita (menor valor 61 contra limite 60).
- **Segunda testemunha.** A família "aprendida" não existe ainda. Sem ela, `decide` não acusa. O pior caso, se uma segunda testemunha sempre concordasse, seriam 3 falsas acusações em 5184 golpes na grade de desenvolvimento.
- **Aprendiz.** Perfil ainda de visitante, FSRS não implementado, nenhum escolhedor aprendido. A agenda de 1/2/4/8/16 dias não foi validada com pessoas.
- **Ainda não verificado:** nenhuma sessão com instrumento real, e a captura visual final do contrato e do aprendiz na tela.
