# Visão geral da arquitetura

Este texto resume como o código está organizado. Ele descreve o que existe hoje. Os planos futuros estão no [design do jogo](../design/design-do-jogo.md) e nos [ADRs](../adr/). Os crates ainda usam o prefixo legado `tonica-`; veja a nota de nomes no [README](../../README.md).

## Fluxo de dados

```text
microfone ──> captura ──> DSP ──> eventos do motor ──> adaptador ──> juiz ──> veredito ──> jogo
(callback)   (anel)     (ataques)  (carimbados)         (sessão)   (puro)   (log)        (Bevy)
                                         │                                      │
                                         └────────────── fita (disco) <─────────┘
caixa de som <── mixer (clique, base) <── comandos da sessão
```

1. **Áudio.** O backend (`tonica-audio`) chama dois callbacks: um de entrada (microfone) e um de saída (caixa de som). Cada bloco leva carimbos de tempo do dispositivo e do relógio do sistema.
2. **DSP.** O estágio de captura roda o detector de `tonica-dsp` sobre cada bloco: filtros, decimação, janela, detector de altura (MPM) e detector de ataque de rasgueado, com datação de cerca de 1 ms.
3. **Eventos do motor.** Uma etapa de análise junta carimbos de entrada e saída num mapa de relógio (`ClockMap`) e converte ataques em eventos com tempo na linha do tempo do jogo.
4. **Sessão.** `tonica-session` recebe os eventos, grava a fita e entrega ao adaptador, que alimenta o juiz.
5. **Juiz.** `tonica-core` decide, para cada evento esperado da fase, se houve ataque no tempo certo. Devolve vereditos como `Ok`, `Errado(tipo)` ou `NaoOuvi`.
6. **Jogo.** `tonica-game` lê a sessão e desenha os vereditos. Lê o relógio da música a partir do relógio de **áudio**, nunca do tempo de quadro.

## Os crates e suas fronteiras

| Camada | Crate | Regra de fronteira |
|---|---|---|
| Sinal | `tonica-dsp` | Independente de motor. Sem alocação no caminho por quadro depois da construção. |
| Teoria | `tonica-theory` | Notas, intervalos, escalas e acordes. Sem áudio. |
| Regras | `tonica-core` | Funções puras. Sem renderização, sem áudio, sem arquivos, sem relógio de parede. |
| Áudio | `tonica-audio` | Só o módulo `cpal_backend` importa `cpal`. O resto vale para qualquer backend. |
| Sessão | `tonica-session` | Liga motor, juiz e fita. Faz o replay. |
| Aprendizagem | `guitarness-learn` | Puro: sem áudio, sem render, sem relógio de parede (o tempo entra como parâmetro). |
| Verificação | `guitarness-verify` | Testemunha DSP para acordes e sua bancada sintética. |
| Teste | `tonica-sim` | Violão, salas e microfones sintéticos, com verdade de referência. |
| Ferramentas | `tonica-cli` | Binário `tonica`. |
| Jogo | `tonica-game` | App Bevy, workspace separado. Nunca bloqueia no áudio nem toca em dispositivo diretamente. |

## Tempo real sem alocação

Os callbacks de áudio seguem estas regras (documentadas em `tonica-audio`):

- Nenhuma alocação, nenhum travamento que possa bloquear e nenhuma impressão.
- Os buffers de trabalho são alocados antes de os fluxos começarem.
- Filas entre threads são anéis (`rtrb`). Anel cheio descarta o item novo e conta o descarte; a etapa de análise avisa com `EngineEvent::Dropped`.

O projeto verifica isso por teste: `tonica-dsp` e `tonica-audio` têm testes `no_alloc` com `assert_no_alloc`, que valem em compilação de depuração. O controle negativo (um `vec!` dentro da guarda aborta o processo) está registrado no [relatório M0 e M1](../relatorios/marcos-m0-m1.md).

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

O comando `tonica replay` reexecuta fitas em dois níveis:

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

Estado atual: `Attack`, `Timing` e `Tendency` estão em `Provisional`, e os outros sete tipos estão em `Locked`. Nenhum está `Open`. O que o microfone pode verificar é calculado na hora, a partir da tabela de portões e da calibração, e nunca é guardado na fase. Detalhes e números no [relatório da fatia Pulso](../relatorios/fatia-pulso.md).

O juiz acusa um erro só com mais evidência do que usa para confirmar um acerto. Quando não há evidência, o resultado é "não ouvi" ou "não julgado", nunca um erro.

## Calibração

`tonica calibrate` mede a latência do laço (alto-falante até microfone, `L_rt`) tocando cliques e ouvindo-os, e separa o viés do jogador do viés do sistema. O resultado fica num perfil por dispositivo. Sem laço (fones, Bluetooth), o jogo usa só uma estimativa e não afirma tendência.

## Jogo

`tonica-game` segue uma convenção: cada recurso tem um plugin de lógica, sem renderização e testável com `MinimalPlugins`, e um plugin de desenho. Cada módulo expõe `install(&mut App, &AppConfig)`.

- O relógio da música (`SongClock`) vem do relógio de áudio, e não do tempo de quadro.
- O modo `--screenshot` roda sem janela e grava PNGs; é assim que as capturas do repositório são geradas.
- A tela do Pulso tem as vistas 2D e 3D, a Passagem de som, o resumo da rodada e a contestação (Backspace).
- Erro aparece em branco ou cinza e por forma, nunca por cor. Não há tela de falha.

## Limites conhecidos

Nada disso foi medido com microfone ou alto-falante reais. Os pontos abertos estão na seção 8 do [relatório da fatia Pulso](../relatorios/fatia-pulso.md). Em resumo: o modo ao vivo só passou em testes sem dispositivo, os portões dependem de fitas reais, e o ADR-002 (reprodução de áudio) continua **proposto**.
