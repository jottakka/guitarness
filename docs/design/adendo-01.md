# Tônica PC — Adendo 01 ao DESIGN v1

Data: 16/11/2026. Origem: decisões de design de 16/11/2026. Este adendo **acrescenta** ao DESIGN v1. Onde houver conflito, vale o adendo.

## A1. Vistas

Três vistas da mesma pista. A tecla V alterna; o padrão é a primeira.

| Vista | Para quê | Como é |
|---|---|---|
| **Braço 3D** (padrão) | ver de longe o que vem; saber onde a mão esquerda vai | O próprio braço do violão em perspectiva. Casas no eixo X (espaçamento como num braço real), cordas empilhadas na altura (Mi grave mais perto). As gemas vêm do fundo pelo eixo Z e pousam em (casa, corda), com uma haste fina até o braço. Faixa iluminada na zona da mão (cerca de 4 casas); a câmera desliza até ela antes da hora (≥ 300 ms, com suavização). Acorde = uma moldura translúcida sobre as casas e cordas usadas; acorde repetido = só o contorno. |
| **Pista 3D** | ritmo puro, leitura rápida | Uma faixa por corda, gemas vindo do fundo (o protótipo atual). |
| **Fita 2D** | leitura tranquila, telas pequenas, Partitura | Linhas horizontais (cordas) e cursor vertical; a música corre da direita para a esquerda. |

Regras das três vistas:

- **Sem texto na tela**, exceto símbolos de acorde (um por compasso, no máximo) e, só na Braço 3D, os números das casas sob a zona da mão (tecla F liga e desliga; ligado por padrão).
- Cores e formas de intervalo do §5. **Erro nunca usa matiz:** marcas de erro são brancas ou cinza e se distinguem pela forma (o ✕ de corda extra é branco, não laranja).
- **Inspiração, não cópia.** Do Rocksmith usamos só convenções de leiaute do gênero (braço em perspectiva, gema com haste, zona da mão, moldura de acorde). Não copiamos logotipo, fontes, interface, nem o esquema de cores das cordas. As cordas são tubos de neon neutros (as graves um pouco mais grossas); a cor fica com o intervalo.

Referências visuais aprovadas: [`highway-3d.png`](../arte/minimal/highway-3d.png), [`lane-2d.png`](../arte/minimal/lane-2d.png), [`timing-3d.png`](../arte/minimal/timing-3d.png), [`timing-2d.png`](../arte/minimal/timing-2d.png), [`maestro-minimal.png`](../arte/minimal/maestro-minimal.png) (sem texto; imagens geradas por IA, veja o [README](../arte/minimal/README.md)).

## A2. Tipos de golpe

Cada tipo tem um glifo próprio, legível de longe e sem texto, e uma regra de juiz.

### Esquema (`chart.json`, ainda `v: 1`; campos novos são opcionais)

```jsonc
{ "tick": 0,    "dur": 240, "kind": "strum", "chord": "G", "dir": "D", "span": [0,5] },
{ "tick": 240,  "dur": 240, "kind": "strum", "chord": "G", "dir": "U", "span": [3,5], "style": "accent" },
{ "tick": 480,  "dur": 240, "kind": "mute",  "dir": "D" },
{ "tick": 720,  "dur": 240, "kind": "strum", "chord": "G", "dir": "D", "span": [0,2] },
{ "tick": 960,  "dur": 480, "kind": "strum", "chord": "C", "dir": "D", "style": "roll", "roll_ms": 150 },
{ "tick": 1440, "dur": 240, "kind": "ghost", "dir": "U" },
{ "tick": 1680, "dur": 240, "kind": "strum", "chord": "E", "dir": "D", "style": "pm" },
{ "tick": 1920, "dur": 240, "kind": "note",  "string": 0, "fret": 3, "finger": "p" }
```

`span` usa a numeração do §4.3 (0 = 6ª corda). Sem `style`, vale `"full"`.

### Tabela

| Tipo | Chart | Glifo | Juiz (Aprendiz / Banda / Palco) |
|---|---|---|---|
| Para baixo | `strum`, `dir: "D"` | seta para baixo que cobre exatamente as cordas do `span` | golpe completo (§1) |
| Para cima | `strum`, `dir: "U"` | seta para cima; costuma cobrir só as agudas | idem; direção só reprova no Palco (§1.4) |
| Acento | `style: "accent"` | moldura mais grossa e mais clara; anel de choque no acerto | golpe completo + intensidade ≥ +4 dB sobre a mediana dos golpes do jogador naquela música. Aprendiz: dica; Banda e Palco: conta |
| Parcial (só graves, só agudas) | `span` restrito | moldura e seta só nas cordas do `span` | cordas fora do `span`: `May` no Aprendiz, `MustNot` a partir da Banda |
| Abafado ("chuck") | `kind: "mute"` | moldura cinza hachurada, sem gemas de altura | ataque obrigatório + timbre abafado (sem altura sustentada; decaimento < 80 ms). Todas as cordas `Muffled` = certo |
| Arpejado (rolado) | `style: "roll"`, `roll_ms` | gemas em escada no tempo, seta curva | cada corda do `span` com ataque próprio, na ordem da direção; `spread_ms` ≥ 0,6·`roll_ms`. Direção julgada (aqui o `spread` é grande o bastante) |
| Fantasma (golpe no ar) | `kind: "ghost"` | seta pontilhada e fraca | **não julgado.** Serve para manter o pêndulo da mão visível. Um ataque ali não é erro (no Palco, vira só dica) |
| Abafado com a palma (PM) | `style: "pm"` | gemas curtas com rabo hachurado | alturas certas + decaimento curto (< 150 ms). Aprendiz: só as alturas |
| Dedilhado | `kind: "note"`, `finger` | gemas de corda única; polegar (`p`) com anel pequeno nas graves | nota única por corda (§1); o dedo não é julgado por áudio |

Princípio 3 continua: o golpe nunca é opcional. O fantasma é a única exceção, e é explícita (não há ataque esperado).

O protótipo (`crates/tonica-game`) ganha uma "galeria de golpes" (`--chart strokes`, tecla C) que mostra todos os tipos a 70 BPM.

## A3. Modo Pulso (treino de tempo)

**Objetivo:** mão direita no tempo, com retorno que informa sem envergonhar. Mede viés (adiantado/atrasado) e consistência (σ). Não depende de acorde: usa golpe abafado.

**Pré-requisito:** Passagem de som passo 2 (`L_rt` e `bias_player`, A4). Se o laço falhar (sem alto-falante, Bluetooth), o jogo usa só golpes abafados sobre o metrônomo: o `bias_player` então absorve a latência inteira, e o ritmo segue julgável se a MAD ≤ 15 ms. Acima disso, o ritmo fica visível e **não julgado**, com o cadeado do §2.

**Escada de exercícios** (cada um com 2 a 4 compassos por rodada; rodadas de 30 a 60 s):

1. **Eco.** O Maestro toca um compasso de golpes abafados; o jogador repete. Julga as razões entre intervalos, não o instante absoluto, por isso não depende de latência. Bom primeiro passo.
2. **No clique.** Semínimas abafadas a 60–90 BPM; depois colcheias baixo/cima.
3. **Metrônomo que some.** O clique toca 2 compassos, some por 2 (ou 4), volta. Mede a deriva durante o silêncio. A pesquisa não achou estudo controlado deste método ([pesquisa de game design](../pesquisa/game-design.md), item 6 do resumo): tratar como **experimento medido**, não como verdade.
4. **Contratempo.** Só os "e" (2º, 4º… colcheias).
5. **Padrões.** `pop8` e outros, primeiro abafados, depois com um acorde parado (ponte para a Estrada).

**Janelas.** As do §1.5 (Aprendiz ±120 ms, Banda ±80, Palco ±50), limitadas a 0,25 do intervalo. "No centro" (±25 ms) só aparece se o laço medir σ ≤ 8 ms.

**Retorno.**

- No ataque (≤ 1 quadro de tela): a seta do golpe enche e se desloca na proporção do erro, adiantada para um lado, atrasada para o outro. Posição, não número.
- No fim da rodada: nuvem de pontos em torno da linha do tempo e um histograma pequeno (como em [`timing-2d.png`](../arte/minimal/timing-2d.png)). **Primeiro o que melhorou** ("mais perto do centro que na última"); a tendência vem depois, em uma frase do Maestro ("você tende a antecipar o 3").
- Números em ms só no painel "Detalhes", fechado por padrão.
- Nada drena e nada se perde: sem vidas, sem tela de fracasso, sem sequência que zera.

**Ajuste.** Duas rodadas com σ abaixo da meta: o andamento sobe 5 BPM, com aviso visível e um botão de voltar. Depois de duas rodadas difíceis, o Maestro **oferece** −10% num chip; nunca baixa sozinho em silêncio (ajuste visível e reversível, ver pesquisa, item 1).

**Vistas.**

- Pulso 3D: aros que chegam pela estrada até a linha de acerto ([`timing-3d.png`](../arte/minimal/timing-3d.png)).
- Pulso 2D: linha do tempo com marcas e pontos acima e abaixo ([`timing-2d.png`](../arte/minimal/timing-2d.png)).

## A4. Detector de ataque e calibração: estado medido

Código: `crates/tonica-dsp/src/{onset,resample,latency}.rs`; bancada em `crates/tonica-sim`.

**Detector (`OnsetDetector`).** Entrada → passa-altas 60 Hz → meia-banda ×2 (24 kHz) → dois ramos:

- Detecção: STFT 1024/128, fluxo espectral log (SuperFlux, atraso 2, filtro de máximo ±1 bin), 70 Hz–5 kHz, limiar mediana + 5·MAD em 0,5 s.
- Tempo e presença: energia acima de 4 cortes (2 / 2,75 / 3,5 / 4,25 kHz, todos até 5 kHz, 60 dB/oit.) em células de 0,5 ms. Por quê: golpe novo é energia nova e brilhante; acorde soando é opaco, porque os parciais altos morrem primeiro. Fórmula por célula: `subida = máx nas 4 bandas de 10·log10((W + F) / (R + F))`.
  - `W`: energia da banda na janela de 2 ms (4 células) que termina na célula.
  - `R = máx(H, M)`. `H`: a maior janela de 2 ms terminando nos 15 ms antes. `M`: máscara de eco, `máx(M·decaimento, janela que sai × 10^(−13/10))`, caindo 100 dB/s.
  - `F = 4·N + g·E_alto + 1e-8`, somado dos dois lados. `N`: piso de ruído da banda (menor média de 32 ms nos últimos 4 s de entrada viva; silêncio digital não conta). `E_alto`: energia acima de 6,5 kHz nas duas últimas janelas, com `g` = −60 dB (vazamento dos sons do jogo). A forma `W / máx(R, F)` foi medida e recusada: quebra `sustain_only` com qualquer `noise_k` de 1 a 4.
- Decisão: o grupo de picos do fluxo (≤ 45 ms entre si, ≤ 180 ms no total) só é golpe se uma célula sobe ≥ 2 dB; o ataque é datado na primeira subida forte. Grupo sem subida é recusado: mudança só da mão esquerda não é golpe.
- `Heard` (aviso visual) sai assim que uma célula sobe ≥ 4 dB, sem esperar o fluxo. Um ataque a 45 ms ou mais do anterior é golpe novo: o grupo aberto fecha (sai o `Strum`) e sai um `Heard` novo. Ataque mais perto é outra corda do mesmo golpe e não ganha segundo aviso. Contrato: cada `Strum` consome exatamente um `Heard` seu; um `Heard` pode ficar sem `Strum`.
- Entrada: amostra não finita vira 0; o resto é limitado a ±4 antes do primeiro filtro.
- Partida: os primeiros 32 ms de um fluxo não geram aviso nem golpe (o piso de ruído ainda não existe). A correção barata dobrou os falsos avisos na partida; fica como limite.
- Sem alocação no caminho por bloco (teste `onset_process_does_not_allocate`). Custo medido: 8,3–8,5 µs em média, máximo 9–14 µs por bloco de 256 amostras a 48 kHz (orçamento 30 µs).

**Bancada sintética** (SNR 40/30/20 dB; reverberação; 30% abafados; 60–180 BPM; clique de metrônomo vazando; 44,1 e 48 kHz). Grade rápida: 99 tomadas, 2376 golpes. Grade completa: 3 sementes, 6984 golpes. Rodado em 16/11/2026 em macOS e Windows. A revisão de código achou 11 problemas; 10 corrigidos com testes de regressão, sem mudar os números de golpe.

| Medida | Portão (§1.11) | Antes (rápida / completa) | Depois (rápida / completa) |
|---|---|---|---|
| golpes achados | ≥ 99,5% | 91,5% / 90,6% | **99,71% / 99,70%** |
| extras | ≤ 0,5% | 0,13% / 0,06% | 0,04% / 0,09% |
| viés | ≤ 2 ms | +0,86 / +0,87 ms | +0,63 / +0,63 ms |
| σ | ≤ 4 ms | 1,46 / 1,51 ms | 0,93 / 0,95 ms |
| p95 do erro | — | 2,31 / 2,35 ms | 1,65 / 1,66 ms |
| erro máximo | — | 17,0 / 25,4 ms | 20,4 / 34,5 ms |
| `Heard` p95 | ≤ 11 ms (portão novo) | 30,3 / 29,9 ms | **3,4 / 3,5 ms** |
| golpes avisados em ≤ 11 ms | ≥ 98% (portão novo) | 0,08% / 0,04% | 98,40% / 98,25% |
| aviso sem golpe | ≤ 0,5% (portão novo) | — | 0 / 2 |
| quebras de contrato (`Strum` sem `Heard` próprio, aviso antes do ataque) | 0 (portão novo) | 6 / 10 | 0 / 0 |

Por andamento (completa, depois): 60 e 100 BPM em colcheias 100%; 120 BPM em semicolcheias 99,07%; 180 BPM em colcheias 99,71%. O erro máximo piorou porque golpes antes perdidos agora são achados, alguns datados numa corda mais tardia.

Regressões sem golpe (13, todas passam, e o número de `Heard` também tem de bater): `no_strum_at_all`, `metronome_leak_no_strum`, `sustain_only`, `left_hand_only`, `left_hand_multi`, `left_hand_early`, `slide`, `vibrato_bend`, `hand_damp`, `fingers_placed`, `fingers_placed_quiet_room`, `metronome_digital_silence_48k` e `_44k` (clique do jogo sobre silêncio digital, achado pelo teste do mixer do M2).

Limites conhecidos:

- **Martelada firme conta como golpe.** Uma martelada (hammer-on) forte é excitação nova, tão alta e brilhante quanto uma palhetada fraca numa corda; não achei evidência que as separe. Fronteira a 30 dB de SNR: cerca de −24 dB (brilhante) é contada, −30 dB é recusada. O teste `firm_hammer_on_is_not_a_strum` fica `#[ignore]` com o motivo. No modo Batida isso só pesa se o jogador martelar entre golpes; nos modos de dedilhado (M4+) vai precisar do contexto do chart.
- Golpes abafados fracos logo depois de golpes fortes em andamento rápido: cerca de 0,3% dos golpes.
- O bloco de entrada soma à latência do `Heard`: 256 quadros somam até 5,3 ms; 480 quadros somam até 10 ms e passariam de 11 ms.
- Portão de ruído na entrada (AGC, supressão): a abertura do portão é ela mesma um ataque. Simulado: extras 1 → 3, σ 0,93 → 1,59 ms.
- O que a bancada não diz: os ataques simulados são ruído branco, então todo golpe é brilhante. Polegar, carne, náilon e cordas velhas podem ter pouca energia em 2–5 kHz. A máscara de eco foi medida contra a reverberação de Schroeder. Intermodulação do alto-falante com os parciais de 8 + 11 kHz do clique cairia em 3 kHz, dentro das bandas; não foi simulada.

**Calibração (`latency.rs`, fora do áudio em tempo real).**

- `L_rt`: sonda de 20 ms (varredura linear 1,5–7 kHz, janela de Hann), tocada 10 vezes; filtro casado em quadratura; chegada = **primeiro** pico do envelope ≥ 50% do maior (o caminho direto, mesmo com reflexão mais forte), com ajuste sub-amostra. Recusa com menos de 7 sondas ouvidas ou MAD > 3 ms. Testes: 87,3 ms numa sala com 4 ecos a 0 dB de SNR, erro < 0,2 ms a 48 e 44,1 kHz; reflexão 1,4× mais forte a 2,5 ms não desloca; jitter de 8 ms é recusado; silêncio é recusado.
- `bias_player`: `e = (ataque − L_rt) − batida` para o golpe mais próximo de cada batida (janela 0,35 do intervalo); mediana e MAD; exige ≥ 12 golpes; `judgeable = MAD ≤ 15 ms`.

**Limites.** Tudo isso é sintético. Violão real, microfone de notebook, AGC do Windows e fone Bluetooth só se medem no M2/M3 (gravações do jogador). O caminho de calibração ao vivo (`cpal`, `ClockMap`, agenda da sonda no mixer) é M2.

## A5. Pesquisa de game design

Relatório completo: [pesquisa de game design](../pesquisa/game-design.md) (22 ideias ranqueadas, cada afirmação marcada como evidência ou inferência, fontes com data). Entram neste adendo: ajuste visível e reversível (A3), retorno que começa pelo que melhorou (A3), metrônomo que some como experimento (A3). Ficam para decisão (A6): nome do modo sem pressão, contador que só cresce em vez de sequência, regras de notificação.

## A6. Decisões novas para o usuário

1. **Modo sem pressão:** chamar de "Sem pressão" (nome neutro, como recomenda a pesquisa) e deixá-lo sempre a um clique?
2. **Acento** julgado a partir da Banda (+4 dB sobre a mediana do jogador), ou só dica em todos os níveis?
3. **Fantasma** visível por padrão (ajuda o pêndulo) ou só na galeria?
4. **Números das casas** ligados por padrão na Braço 3D?
5. **Dias tocados:** só um contador que cresce, ou também uma "sequência" que nunca zera (pausa em vez de quebrar)?
