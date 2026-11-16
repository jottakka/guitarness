# Visão geral da arquitetura

Este texto resume como o código está organizado. Ele descreve o que existe hoje. Os planos futuros estão no [design do jogo](../design/design-do-jogo.md) e nos [ADRs](../adr/). Os crates ainda usam o prefixo legado `guitarness-`; veja a nota de nomes no [README](../../README.md).

## Fluxo de dados

```text
microfone ──> captura ──> DSP ──> eventos do motor ──> adaptador ──> juiz ──> veredito ──> jogo
(callback)   (anel)     (ataques)  (carimbados)         (sessão)   (puro)   (log)        (Bevy)
                                         │                                      │
                                         └────────────── fita (disco) <─────────┘
caixa de som <── mixer (clique, base) <── comandos da sessão
```

1. **Áudio.** O backend (`guitarness-audio`) chama dois callbacks: um de entrada (microfone) e um de saída (caixa de som). Cada bloco leva carimbos de tempo do dispositivo e do relógio do sistema.
2. **DSP.** O estágio de captura roda o detector de `guitarness-dsp` sobre cada bloco: filtros, decimação, janela, detector de altura (MPM) e detector de ataque de rasgueado, com datação de cerca de 1 ms.
3. **Eventos do motor.** Uma etapa de análise junta carimbos de entrada e saída num mapa de relógio (`ClockMap`) e converte ataques em eventos com tempo na linha do tempo do jogo.
4. **Sessão.** `guitarness-session` recebe os eventos, grava a fita e entrega ao adaptador, que alimenta o juiz.
5. **Juiz.** `guitarness-core` decide, para cada evento esperado da fase, se houve ataque no tempo certo. Devolve vereditos como `Ok`, `Errado(tipo)` ou `NaoOuvi`.
6. **Jogo.** `guitarness-game` lê a sessão e desenha os vereditos. Lê o relógio da música a partir do relógio de **áudio**, nunca do tempo de quadro.

### Aprendiz, contrato e verificador

Três ramos saem do juiz e dos portões, e nenhum deles muda um veredito:

```text
veredito final ──> evidência ──> registro do bloco ──> domínio (agenda oculta) ──> oferta do próximo bloco
(guitarness-core)     (learn)        (learn)           └─> placar exibido (só sobe)      (learn, jogo)
                                                   └─> learn.jsonl (thread de escrita)

gráfico + portões + calibração ──> contrato do evento ──> "julgado" ou "guia" ──> marcas e instruções (jogo)
                                    (guitarness-core)

fita ──> janela do golpe ──> testemunha DSP ──> decisão ──> relatório de `guitarness verify`
                             (verify)           (verify)    (CLI, só relata)
```

- **Aprendiz.** `guitarness-learn` transforma o veredito final de cada evento em evidência com peso (julgado 1, provisório 0,5, contestado, "não ouvi" ou "não julgado" 0). Alimenta o domínio por habilidade e o placar exibido, e o seletor escolhe uma oferta por vez. O jogo grava as linhas numa thread própria, nunca no quadro.
- **Contrato.** `guitarness-core::contract` separa o que o evento **pede** do que o microfone consegue **julgar**. O que é só pedido vira guia, sem marca de resultado.
- **Verificador.** `guitarness-verify` confere acordes em fitas. Nesta rodada ele só é usado por `guitarness verify`; o jogo não o chama.

Detalhes e números no [relatório da rodada 2](../relatorios/rodada-2.md).

## Os crates e suas fronteiras

| Camada | Crate | Regra de fronteira |
|---|---|---|
| Sinal | `guitarness-dsp` | Independente de motor. Sem alocação no caminho por quadro depois da construção. |
| Teoria | `guitarness-theory` | Notas, intervalos, escalas e acordes. Sem áudio. |
| Regras | `guitarness-core` | Funções puras. Sem renderização, sem áudio, sem arquivos, sem relógio de parede. |
| Áudio | `guitarness-audio` | Só o módulo `cpal_backend` importa `cpal`. O resto vale para qualquer backend. |
| Sessão | `guitarness-session` | Liga motor, juiz e fita. Faz o replay. |
| Aprendizagem | `guitarness-learn` | Puro: sem áudio, sem render, sem relógio de parede (o tempo entra como parâmetro). Depende de `guitarness-core`. Só o armazenamento toca arquivos, e o perfil de visitante não toca. |
| Verificação | `guitarness-verify` | Conjuntos de confusão, testemunha DSP para acordes, regra de decisão e bancada sintética. Nenhum veredito nasce aqui sem portão aberto. |
| Teste | `guitarness-sim` | Violão, salas e microfones sintéticos, com verdade de referência. |
| Ferramentas | `guitarness-cli` | Binário `guitarness`. |
| Jogo | `guitarness-game` | App Bevy, workspace separado. Nunca bloqueia no áudio nem toca em dispositivo diretamente. |

## Tempo real sem alocação

Os callbacks de áudio seguem estas regras (documentadas em `guitarness-audio`):

- Nenhuma alocação, nenhum travamento que possa bloquear e nenhuma impressão.
- Os buffers de trabalho são alocados antes de os fluxos começarem.
- Filas entre threads são anéis (`rtrb`). Anel cheio descarta o item novo e conta o descarte; a etapa de análise avisa com `EngineEvent::Dropped`.

O projeto verifica isso por teste: `guitarness-dsp` e `guitarness-audio` têm testes `no_alloc` com `assert_no_alloc`, que valem em compilação de depuração. O controle negativo (um `vec!` dentro da guarda aborta o processo) está registrado no [relatório M0 e M1](../relatorios/marcos-m0-m1.md).

## Backend falso e determinismo

`FakeBackend` implementa o mesmo trait `Backend` do `cpal`, mas usa um relógio virtual: sem placa de som, sem thread, sem tempo real. O teste avança o tempo e cada callback devido roda na thread do teste, em ordem. A mesma semente dá os mesmos carimbos e amostras. O backend falso modela deriva de relógio, latência acústica, estouros (xruns) e ruído.

Dois resultados dependem disso:

- Testes de tempo, calibração e sessão rodam sem hardware e de forma repetível.
- O jogo roda em modo falso, que é como a fatia Pulso funciona hoje.

## Fita e replay determinístico

O juiz do Pulso é uma função pura da sequência de eventos do motor e dos comandos da sessão. Ele não lê relógio de parede. Por isso, uma sessão gravada pode ser reexecutada com o mesmo resultado.

Uma **fita** é uma pasta com um recorte de uma época do motor:

| Arquivo | Conteúdo |
|---|---|
| `tape.json` | Cabeçalho: versão, sistema, taxas, perfil, referência de tempo, configuração do juiz, portões, contestações e se a fita está completa. |
| `mic.wav` | Áudio mono `f32` que o detector viu. |
| `mic.runs.json` | Trechos contínuos do áudio e blocos perdidos. |
| `events.jsonl` | Eventos do motor e saídas do juiz, uma linha por evento. |

O comando `guitarness replay` reexecuta fitas em dois níveis:

- `Events`: os eventos gravados passam por um adaptador e um juiz novos. O resultado é comparado com o gravado.
- `Audio`: o `mic.wav` passa pelo detector de ataque de novo, e os ataques são comparados um a um.

Uma fita incompleta (queda, perda do anel cru ou dano no arquivo) fica fora dos portões e é listada pelo nome. Uma fita contestada nunca é apagada.

## Portões de precisão

Cada tipo de veredito tem um **portão** com três estados:

| Estado | Significado |
|---|---|
| `Locked` | O juiz não julga esse tipo. Responde "não julgado". É o padrão de todo tipo sem evidência. |
| `Provisional` | O juiz julga, com evidência só sintética (jogador virtual na integração contínua). |
| `Open` | O juiz julga, com respaldo de fitas reais suficientes. **Só fita real abre um portão.** |

Estado atual: `Attack`, `Timing` e `Tendency` estão em `Provisional`, e os outros sete tipos estão em `Locked`. Nenhum está `Open`. Evidência sintética nunca abre os sete tipos que não são de tempo: os de acorde só abrem com fita real. O que o microfone pode verificar é calculado na hora, a partir da tabela de portões e da calibração, e nunca é guardado na fase. Detalhes e números no [relatório da fatia Pulso](../relatorios/fatia-pulso.md).

O juiz acusa um erro só com mais evidência do que usa para confirmar um acerto. Quando não há evidência, o resultado é "não ouvi" ou "não julgado", nunca um erro.

## Calibração

`guitarness calibrate` mede a latência do laço (alto-falante até microfone, `L_rt`) tocando cliques e ouvindo-os, e separa o viés do jogador do viés do sistema. O resultado fica num perfil por dispositivo. Sem laço (fones, Bluetooth), o jogo usa só uma estimativa e não afirma tendência.

## Jogo

`guitarness-game` segue uma convenção: cada recurso tem um plugin de lógica, sem renderização e testável com `MinimalPlugins`, e um plugin de desenho. Cada módulo expõe `install(&mut App, &AppConfig)`.

- O relógio da música (`SongClock`) vem do relógio de áudio, e não do tempo de quadro.
- O modo `--screenshot` roda sem janela e grava PNGs; é assim que as capturas do repositório são geradas.
- A tela do Pulso tem as vistas 2D e 3D, a Passagem de som, o resumo da rodada e a contestação (Backspace).
- O aprendiz está ligado (placar, oferta única de bloco, tecla `O` para outra coisa). O perfil ainda é de visitante, sem arquivo.
- Cada evento tem um contrato: o julgado usa as marcas de sempre, e o que só é pedido aparece como instrução em cinza.
- Erro aparece em branco ou cinza e por forma, nunca por cor. Não há tela de falha.

## Limites conhecidos

Nada disso foi medido com microfone ou alto-falante reais. Os pontos abertos estão na seção 8 do [relatório da fatia Pulso](../relatorios/fatia-pulso.md). Em resumo: o modo ao vivo só passou em testes sem dispositivo, os portões dependem de fitas reais, e o ADR-002 (reprodução de áudio) continua **proposto**.
