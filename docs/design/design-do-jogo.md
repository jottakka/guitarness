# Tônica PC — DESIGN v1

> **Adendo 01 (16/11/2026):** vistas Braço 3D / Pista 3D / Fita 2D, tipos de golpe, modo Pulso (treino de tempo), estado medido do detector de ataque e da calibração. Ver [adendo 01](adendo-01.md).

Novembro de 2026. Base: os relatórios de pesquisa em [`../pesquisa/`](../pesquisa/). Nada aqui foi medido: todo número é **alvo** ou **portão**; (U) marca dado de pesquisa não verificado.

## 0. Princípios

1. **Verificar, não transcrever.** O jogo sabe o que espera; o juiz testa "soou o esperado?" contra um conjunto fechado de erros plausíveis. Tablatura automática não tem precisão para reprovar corda.
2. **Três veredictos:** `Ok`, `Errado(tipo)`, `NaoOuvi`. Acusar exige mais evidência do que confirmar. `NaoOuvi` é visível, contado e limitado por portão.
3. **Nada é opcional em silêncio.** Cada evento exige o seu próprio ataque e a sua própria re-excitação de cordas; acorde ainda soando nunca satisfaz o evento seguinte. O que um nível não julga aparece como "não julgado".
4. LLM fora do julgamento. Limiar do juiz só muda com o jogador vendo.

## 1. Juiz estrito

### 1.1 Evento esperado

Cordas `s = 0..5` (0 = 6ª, como na demo). Cada evento do chart vira um `Expect`:

```rust
#[repr(u8)] pub enum Req { Must, May, MustNot }
pub struct StringSpec { pub req: Req, pub fret: i8 }            // -1 = sem casa
pub enum ExpectKind { Note, Strum, Mute, Rest }
pub struct Expect { pub id: u32, pub t_exp: SampleTime, pub kind: ExpectKind,
    pub strings: [StringSpec; 6], pub dir: Dir, pub win: TimingWin, pub dict: DictId }
```

`Must`: precisa soar na casa indicada. `May`: pode soar ou não (cordas graves num golpe para cima; G sem a 5ª corda). `MustNot`: não pode soar (6ª solta em D). Quem define é a tabela de pegadas e o `span` do golpe (4.3), nunca o juiz. Nota única e dedilhado são o mesmo teste, com uma corda `Must`.

### 1.2 O que conta como certo

| Objeto | `Ok` | `Errado` (confiante) | `NaoOuvi` |
|---|---|---|---|
| Nota única | ataque casado; o átomo esperado ganhou energia depois do ataque (`presence` do M1 ou MPM com clareza ≥ `C_ON`), a ±35 cents da afinação calibrada, com as travas de oitava | outra nota limpa por 2 quadros → `WrongNote(midi)` (oitava, corda vizinha, casa ao lado, solta) | ataque sem altura clara; SNR baixo |
| Corda da nota (mesma altura, outra corda) | só no Palco, com o portão de 1.11 | idem | padrão: "corda: guia" |
| Identidade do acorde | baixo = classe esperada; toda classe de nota do acorde presente; nenhuma classe estranha re-excitada | uma hipótese do conjunto de confusão vence a esperada por ≥ `T_err` → `WrongChord(id)`; tudo abafado → `WrongChord(Muted)` | margem na zona morta |
| Tempo | `abs(e) ≤ W` do nível (1.5) | sem ataque até `t_exp + W` → `Miss`; ataque sem evento → `ExtraStrum` | ritmo não calibrado → "não julgado" |
| Direção | sentido ouvido = esperado, com confiança | sentido oposto, com confiança | rasgueado rápido demais para ordenar as cordas |

Cordas do acorde (`NaoOuvi` = LLR na zona morta):

| Esperado | Observado no golpe | Veredicto |
|---|---|---|
| `Must(f)` | re-excitada em `f`, sustentando | `Ok` |
| `Must(f)` | sem energia nova | `Missing` |
| `Must(f)` | golpe com decaimento curto, sem parciais estáveis | `Muffled` |
| `Must(f)` | re-excitada em `f' ≠ f` | `WrongFret(f')` |
| `MustNot` | re-excitada em qualquer casa | `Extra` |
| `MustNot` | silêncio ou golpe abafado (polegar abafando) | `Ok` |
| `May` | qualquer coisa, exceto classe de nota estranha | `Ok` |

### 1.3 Algoritmo em camadas

**C0, quadro (callback).** Passa-altas 60 Hz, FIR polifásico para `fs_a` = 24 kHz, passo de 128 amostras (5,33 ms) depois da paridade. STFT de 1024; fluxo espectral (log-magnitude, meia-onda, 70 Hz a 5 kHz) com limiar mediana + k·MAD de 0,5 s e um passo de espera. MPM e `presence` do M1 continuam aqui.

**C1, segmentação e casamento (thread do juiz).** Rasgueado = picos de fluxo separados por ≤ 40 ms, com duração ≤ 180 ms. `t_onset` é retro-datado para a maior subida da envoltória (~1 ms); `spread_ms` vai do primeiro ao último pico. Casamento com `Expect`: evento mais próximo dentro da janela, um para um, em ordem.

**C2, teste de excitação (o núcleo).**

- **Dicionário por evento**, construído 2 s à frente, nunca na hora do veredicto. Átomo = (corda, casa). Entram os esperados; por corda `Must`, as alternativas {solta, `f−1`, `f+1`, `f+2`, casa do acorde anterior, casa do próximo}; por corda `MustNot`, {solta, casa do acorde anterior}. Até 32 átomos, cada um em duas colunas (parciais 1 a 4; 5 em diante), para o ajuste não depender do brilho do toque.
- **Coluna** = soma de parciais em `f_h = h·f0·√(1 + B_s·h²)`, com amplitude `env_s(f_h)·pluck_s[h]`, espalhada pelo núcleo da janela. `f0`, `B_s`, `env_s` e `pluck_s` vêm da calibração do violão do jogador.
- **Observação:** espectro de magnitude ponderado por 1/piso de ruído, FFT de 4096 (5,86 Hz por bin), 70 Hz a 5 kHz, em três janelas: `pre` (termina 5 ms antes do ataque), `early` (começa 10 ms depois do fim do rasgueado) e `late`.
- **Ajuste:** NNLS por descida de coordenadas sobre `G = DᵀD` (pré-calculada) e `b = Dᵀx`.
- **Estatística por átomo:** `g = N_ef · ln(RSS_sem_átomo / RSS_completo)`, com re-ajuste quente sem o átomo. É a razão de verossimilhança de modelos aninhados: "incluir esta corda nesta casa melhora o ajuste?".
- **Features por corda:** `g_exp`; `g_alt` (melhor alternativa); `gain_db` (`early` contra `pre` decaído: separa corda golpeada agora de corda que só continuou soando e de ressonância por simpatia); `decay_db` (`early`→`late`: abafada contra soando); `rel_db` (nível relativo à mediana do golpe); `obs` (observabilidade pré-calculada: fração da energia do átomo em bins que nenhum outro átomo domina).
- **LLR calibrada:** `ℓ = w·φ + b`, regressão logística por grupo de corda (bordões, primas). Os pesos saem da bancada; o viés é adaptado ao jogador. Saem `ℓ_ring`, `ℓ_alt` e `ℓ_dead` por corda.
- **Decisão sequencial (Wald).** `T_ok = ln((1−β)/α)` e `T_err = ln((1−α)/β)`, com α = erro aceito e β = acerto reprovado. A janela cresce 64 → 96 → 128 ms (171 ms se couber antes do próximo evento) e a decisão para quando um limiar é cruzado. No prazo (próximo evento menos guarda, ou 200 ms), o que sobrou vira `NaoOuvi`.
- **Identidade, caminho B:** croma com soma harmônica + baixo, comparados com o modelo pessoal de cada acorde do conjunto de confusão (gravado no Raio-X). Soma-se à LLR das cordas e segura a identidade quando o ajuste por corda está incerto.

**C3, rede pequena e causal (opcional).** `StringNet`: entram os três espectros em 216 bins log (36 por oitava) e a pegada esperada; sai, por corda, {soando-esperada, silêncio, abafada, outra casa}. Cerca de 60 mil parâmetros, em `tract-onnx`, thread de baixa prioridade, prazo de 150 ms. Treino com golpes sintéticos e GuitarSet (1.11); cabe em CPU, sem depender do broker de GPU. Fusão: `ℓ = ℓ_dsp + λ·ℓ_nn`. **A rede sozinha nunca reprova:** `Errado` exige `ℓ_dsp` também negativa. Só entra se, em dados reais separados, cortar ≥ 30% do erro aceito mantendo reprovação falsa ≤ 2% (ADR-004).

**C4, câmera (opcional, só direção).** Fluxo óptico vertical numa região de 160×120 sobre a mão direita, sem modelo de pose, sincronizado por um golpe de calibração.

### 1.4 Direção (baixo/cima)

Áudio: instante de início por corda (energia do átomo em janelas de 256 a cada 64 amostras) e correlação de postos entre índice da corda e instante: o sinal dá o sentido, o módulo a confiança. Só é audível com `spread_ms` ≥ ~15 ms. A literatura dá ~80 a 90% em microfone (U): insuficiente para reprovar. Direção só reprova no Palco, com câmera ou com o áudio passando no portão de 1.11; nos outros níveis é dica. O padrão alternado baixo/cima nunca conta como evidência.

### 1.5 Tempo

- Tudo em amostras do relógio de entrada; o `ClockMap` (deslocamento + deriva, pelos carimbos do `cpal`) converte entre entrada, saída e chart.
- `e = (t_onset − L_rt) − t_batida_agendada − bias_player`. `L_rt` é a ida e volta medida por clique em laço (10 cliques; recusa se MAD > 3 ms). `bias_player` é a mediana de 8 compassos de golpe abafado sobre o metrônomo: absorve o hábito do jogador e a latência do fone.
- `t_onset` = primeiro ataque do grupo. `spread_ms` > 120 ms vira a dica "rasgueado arrastado".
- Janelas `W`: Aprendiz ±120 ms, Banda ±80 ms, Palco ±50 ms, limitadas a 0,25 do intervalo entre eventos. "No centro" = ±25 ms, exibido só se o laço medir σ ≤ 8 ms.
- Fone Bluetooth: sem calibração por golpe com MAD ≤ 15 ms, o ritmo fica "não julgado", visível.

### 1.6 Calibração: "Passagem de som"

É a primeira fase do jogo, com o Maestro; cada corda acende um tubo de neon.

| Passo | O jogador toca | Tempo | Calibra |
|---|---|---|---|
| 1 | silêncio | 3 s | piso de ruído por bin |
| 2 | nada (cliques) ou 8 compassos abafados | 10 a 25 s | `L_rt`, `bias_player` |
| 3 | cada corda solta 3× (fraco, médio, forte) | 45 s | `tune_cents`, `B_s`, `env_s`, `pluck_s`, decaimento, faixa de nível |
| 4 | cada corda na casa 3 | 20 s | variação do molde com a casa |
| 5 | Em e D: corda a corda, depois golpe lento, normal e para cima | 50 s | distribuições positivas, `spread`, direção |
| 6 | Em só nas 4 cordas agudas; golpe todo abafado | 20 s | distribuições negativas (inclui simpatia), `Muffled` |

Os passos 1 a 3 (1 min) liberam o Aprendiz; 4 a 6 (1,5 min) liberam Banda e Palco. O afinador diário (20 s) refaz o passo 3 reduzido. Veredictos muito confiantes atualizam os moldes devagar, com deriva limitada.

### 1.7 Conjuntos de confusão

Gerados da tabela de pegadas: acordes cuja pegada difere em ≤ 2 cordas, acordes com ≥ 2 classes em comum, o acorde anterior (troca atrasada), o próximo (troca adiantada) e "tudo solto" (esqueceu a mão esquerda).

| Esperado | Confunde com | O que decide |
|---|---|---|
| C | Am, C/G, Cmaj7, 6ª solta (C/E) | 5ª corda C3×A2; 3ª corda G3×A3; átomo G2 ou E2 na 6ª |
| D | Dsus2, Dsus4, Dm, **6ª solta**, 5ª solta | 1ª corda; E2 tem parciais quase todos exclusivos (caso fácil, mesmo sem a fundamental no microfone); A2 é `May` só no Aprendiz |
| G | Em, G7, G sem a 5ª corda | baixo G2×E2; a 1ª corda fica mascarada por oitavas (caso difícil) |
| E / A | Em, E7 / Am, A7, Asus2 | uma corda, uma casa: a terça |
| F | Fmaj7 (1ª solta), F pequeno, 2ª abafada na pestana | E4×F4 na 1ª; `Muffled` na 2ª |
| qualquer | acorde anterior ou próximo | a pegada inteira difere: tem de sair confiante |

O caso estruturalmente difícil é a corda que soa a oitava de outra do mesmo acorde (1ª corda em G, Em, E): seus parciais coincidem com os das graves. A evidência vem do excesso sobre o molde e dos parciais acima de ~2,5 kHz, onde os bordões já não têm energia; daí `obs` por (pegada, corda).

### 1.8 Contra o falso erro

1. Limiares assimétricos: acusar custa mais evidência que confirmar.
2. `NaoOuvi` é um "?" cinza: não mexe no combo e não conta como acerto nem como erro no domínio.
3. Portão de sinal: golpe perto do piso de ruído ou saturado vira `NaoOuvi(baixo|saturou)` com o medidor de nível piscando; nunca `Errado`.
4. Autossuspeita: corda que acumula `Missing` muito acima do esperado, com a identidade passando, faz o juiz oferecer um Raio-X de 20 s para conferir e recalibrar.
5. **Contestar** (pedal, tecla ou voz): salva ±1 s de áudio e o rastro do juiz como caso de regressão; nos modos de treino, concede o evento.

### 1.9 Níveis de rigor

| | Aprendiz | Banda | Palco |
|---|---|---|---|
| Ataque e re-excitação por evento | sempre | sempre | sempre |
| Identidade do acorde | sim | sim | sim |
| `Missing` / `WrongFret` | cordas da tônica e da terça | todas as `Must` | todas |
| `Extra` | cordas abaixo da tônica com nota estranha | todas as `MustNot` | todas |
| `Muffled` | dica | dica | erro |
| Direção | guia | dica | erro, com portão |
| `ExtraStrum` | mostrado | erro | erro |
| α / β nominais | 0,10 / 0,005 | 0,05 / 0,01 | 0,02 / 0,01 |
| `T_ok` / `T_err` | 2,3 / 5,2 | 3,0 / 4,6 | 3,9 / 4,6 |

O que o nível não julga aparece apagado, com cadeado.

### 1.10 Orçamento e estruturas

| Etapa | Onde | Alvo |
|---|---|---|
| Filtro, decimação, STFT 1024, fluxo, MPM, `presence` | callback | ≤ 30 µs por quadro (meta de desempenho) |
| `Dᵀx` + NNLS + re-ajustes, por janela | juiz | ≤ 150 µs |
| Veredicto completo de um golpe (3 janelas, SPRT) | juiz | ≤ 500 µs p99 |
| `StringNet` | thread ML | ≤ 5 ms, prazo 150 ms |
| Ataque acústico → `OnsetHeard` | | ≤ 11 ms + latência de entrada |
| Fim do golpe → veredicto das cordas | | 75 ms (claro) a 180 ms (ambíguo) |

Sem alocação depois da inicialização: `DictPool` com 64 dicionários pré-alocados (64 colunas × 1024 bins, Gram 64×64), anel de 1 s de amostras, filas `rtrb`. Mensagens `Copy` de tamanho fixo:

```rust
#[repr(u8)] pub enum StringState { Ok, Missing, Muffled, WrongFret, Extra, Unsure, NotJudged }
pub struct StringVerdict { pub state: StringState, pub llr: f32, pub heard_fret: i8, pub rel_db: f32 }
pub struct StrumVerdict { pub expect_id: u32, pub t_onset: SampleTime, pub dt_ms: f32,
    pub timing: Timing, pub chord: ChordVerdict, pub strings: [StringVerdict; 6],
    pub dir: DirVerdict, pub spread_ms: f32, pub level_db: f32, pub flags: VerdictFlags }
```

### 1.11 Aceite mensurável

FA = erro real que saiu `Ok`. FR = toque correto que saiu `Errado`. NO = fração de `NaoOuvi`. Toda taxa sai com limite superior de 95%; zero erros em 150 tentativas sustenta "≤ 2%" (regra do três).

**(a) Bancada sintética (`tonica-sim`, CI).** Karplus-Strong por corda com dispersão (inarmonicidade), posição da palhetada, corpo por soma de modos (sem IR de terceiros), simpatia pelo cavalete, ruído rosa + 60 Hz, microfone de notebook (corte em 150 Hz). Grade: 9 acordes (C, D, E, G, A, Am, Em, Dm, F) × golpe (baixo, cima, lento 120 ms, rápido 15 ms) × 3 dinâmicas × erro injetado (nenhum; cada `Must` ausente, abafada, casa ±1, solta; cada `MustNot` golpeada; acorde do conjunto de confusão; acorde anterior) × contexto (silêncio; sobre acorde soando) × SNR (40, 30, 20 dB) × desafinação ±10 cents × 3 violões virtuais, cada um calibrado pelo fluxo de 1.6.

**(b) Bases públicas.** GuitarSet (captador hexafônico: corda e casa por nota; usar o canal de microfone) e IDMT-SMT-Guitar (acordes). Positivos: julgar com esperado = verdade. Negativos: **perturbar a hipótese** (tirar corda, somar corda, casa ±1 ou ±2, trocar pelo acorde confundível) sobre o mesmo áudio. Sem calibração do violão, é limite inferior. Primeiro medir (regra: medir primeiro); o ADR-003 fixa as metas depois. Licenças a conferir antes de baixar.

**(c) Violão do jogador.** `tonica-cli record-script`, 10 min: 8 acordes × 20 golpes corretos (160) + 8 acordes × 8 erros pedidos × 2 (128; ex.: "sem a 1ª", "com a 6ª solta", "Am no lugar de C"). O rótulo é o pedido, confirmado por pedal. Vira regressão, no microfone do notebook e num USB. Com esse tamanho os portões valem no agregado; por tipo de erro há só tendência, até as contestações engrossarem o conjunto.

| Julgamento | Sintético: portão de CI (FA / FR / NO) | Jogador: portão para ligar o julgamento |
|---|---|---|
| Presença do golpe | 0,5% / 0,5% / — | 1% / 1% / — |
| Identidade do acorde | 0,5% / 0,5% / 2% | 2% / 2% / 5% |
| Corda, `obs` alta | 2% / 1% / 5% | 5% / 2% / 10% |
| Corda, `obs` baixa (oitava mascarada) | 10% / 1% / 20% | FR ≤ 2% obrigatório; FA medido; acima de 15%, a corda fica "difícil de ouvir" e só é julgada no Raio-X |
| `Extra` em bordão abaixo da tônica | 1% / 1% / 3% | 3% / 2% / 5% |
| Tempo | viés ≤ 2 ms, σ ≤ 4 ms | σ do laço ≤ 8 ms |
| Direção só por áudio | medir | ≥ 97% nos confiantes com cobertura ≥ 70%; senão desligada |
| Corda da nota única | medir | ≥ 95%; senão "guia" |

Regressões obrigatórias do princípio 3: `no_strum_at_all`, `sustain_only` e `metronome_leak_no_strum` dão `Miss`; `soft_strum_low_snr` dá `NaoOuvi`, nunca `Errado`.

### 1.12 O que não sabemos antes de medir

- A precisão nas cordas mascaradas por oitava, e quanto os moldes variam com palheta, dedo, posição e idade das cordas.
- Quanto o microfone de notebook e os "aprimoramentos" de áudio do Windows (AGC, redução de ruído) destroem a evidência; o modo cru do WASAPI no `cpal` 0.18 não está confirmado.
- A direção só por áudio, e a distribuição real de `spread_ms` deste jogador.
- Se a `StringNet` acrescenta algo, e se a LLR sai bem calibrada fora do sintético.
- Náilon contra aço (inarmonicidade e decaimento mudam) e o efeito do vazamento do alto-falante.

## 2. Modos

Todo modo é uma **lição**: `Lesson { skills, mode, params, judge_profile, pass, records }`. O mapa de domínio é indexado por habilidade (`chord:G`, `string:G/1`, `change:G>C`, `pattern:pop8`, `note:s2f3`, `read:staff:E4`) e alimenta a revisão espaçada do diretor por regras.

| Modo | Habilidade medida | Juiz | Grava |
|---|---|---|---|
| **Estrada** (música + batida) | acordes, trocas, batida, tempo | golpe completo | `StrumVerdict` por evento; tempo de troca; viés e σ de ritmo |
| **Partitura** (pauta + tab) | leitura, dedilhado, ritmo escrito | nota, notas, golpe | acerto por símbolo, por (corda, casa) e por figura; tempo até acertar |
| **Caça-Notas** (3 estilos) | notas e intervalos no braço | nota única | como na demo |
| **Raio-X do Acorde** (era Monta-Acorde) | pegada limpa | nota com alvo, depois golpe | estado por corda; modelo pessoal do acorde |
| **Um Minuto de Trocas** | troca entre dois acordes | identidade + cordas | trocas `Ok` por minuto; o número só sobe |
| **Batida** | mão direita, sem mão esquerda | `Mute` + tempo + direção | viés por célula do padrão |
| **Palco livre** (era Jam) | improviso na pentatônica | nota única | notas dentro do tom |

**Estrada.** Pista até a linha de acerto; cartão de acorde com seta de golpe em cada subdivisão. Até 3 camadas por música: Baixo (tônica no tempo 1), Batida, Dedilhado ou riff. "Ensaio" espera nas trocas; "Show" corre no relógio. Laço de trecho, andamento de 50 a 100%.

**Partitura.** Pauta com tablatura embaixo e cursor no relógio de áudio; laço [A, B], andamento de 40 a 120%, contagem de um compasso. **Espera**: o cursor para em cada evento até ouvir o conjunto certo; mede tempo até acertar, sem julgar ritmo. **No tempo**: julga tudo. Escada de leitura: tab → tab + pauta → só pauta. Dois laços limpos: sobe 5%, com aviso. Abaixo de 60%: o diretor oferece −10% num chip.

**Do veredicto ao retorno.**

- No ataque (≤ 1 quadro de tela): a seta do golpe enche; adiantado ou atrasado desloca a seta na proporção de `dt_ms` (posição, não número); "no centro" brilha.
- No veredicto das cordas: painel de seis cordas de neon na linha de acerto. `Ok` = a corda acende na cor e na forma do intervalo. `Missing` = contorno oco. `Muffled` = traço curto tracejado. `Extra` = ✕. `WrongFret` = seta ◀ ou ▶ para o lado da casa certa. `NaoOuvi` = "?" pontilhado. `NotJudged` = apagado com cadeado.
- Acorde errado: o cartão mostra o que foi ouvido ("ouvi Am").
- **Erro nunca usa matiz:** branco ou cinza, mais forma; o ✕ laranja do prompt de arte perde a cor.
- Nada drena: o combo pausa e a energia do público só sobe.

## 3. Agente

### 3.1 Ajuda em tempo real

Entrada: texto ou **apertar para falar** (pedal HID ou barra de espaço); enquanto apertado, o juiz suspende e o compasso mostra "julgamento em pausa".

1. **Determinístico primeiro.** A fala passa pelo Vosk small-pt em gramática fechada (as frases são os comandos); texto digitado passa por ~40 padrões. Sai uma chamada de ferramenta, sem LLM, em menos de 200 ms.
2. **LLM só no que sobrar.** Fala livre ("essa parte do refrão tá difícil") é transcrita e vai ao cérebro `helper`, com estado compacto (modo, andamento, camada, acerto dos últimos 8 compassos) e os esquemas. Ele devolve no máximo uma chamada e uma frase. Sem resposta a tempo: "diga mais devagar, mais rápido, mais fácil ou repete".

Ferramentas: lista fechada, validada no `tonica-core`. Cada chamada vira um `PendingChange` aplicado **no próximo compasso**, com chip visível antes de valer e "desfaz".

| Ferramenta | Limites |
|---|---|
| `set_tempo_scale(pct \| delta)` | 40 a 120% (50 a 100% com áudio do jogador); passo ≤ 10%; uma mudança a cada 2 compassos |
| `set_tier(1..3 \| ±1)` | troca a camada do chart; não toca no juiz |
| `simplify_chord(chord, voicing_id)` | só pegadas da tabela; vira outro cartão, julgado com o mesmo rigor |
| `loop_section(id \| last_bars 1..8)`, `clear_loop`, `set_wait_mode`, `set_metronome`, `pause`, `resume(count_in)`, `restart_section` | só trechos e valores do chart |
| `explain_last_error` | o núcleo escolhe os fatos; o professor redige até 2 frases |
| `contest_last` | 1.8 |
| `propose_strictness(level)` | só **propõe**: um cartão mostra o que muda e o jogador confirma |

Nenhuma ferramenta altera limiar. O diretor por regras também só oferece chips.

### 3.2 Montador de músicas

Entradas, da mais segura para a menos:

1. **Colar a cifra** ou soltar um arquivo (`.cho`, `.txt`, GP3/4/5, MusicXML, MIDI).
2. **Só o nome:** o arranjador rascunha os acordes pelo que o modelo sabe, marcado "rascunho, confira". Nunca pede nem gera letra.
3. **Buscar uma página** (desligado por padrão): um GET de uma URL informada pelo jogador, respeitando `robots.txt`, sem busca nem varredura, com aviso de que os termos do site são responsabilidade dele.

Nada protegido entra no repositório. Os charts ficam só nos dados locais, sem exportação (exceto `origin = "own"`). Texto importado é dado, nunca instrução.

Esteira: analisadores determinísticos próprios (ChordPro, cifra por linha de acordes, tab ASCII) → `SongDraft`. O LLM estrutura texto bagunçado e escolhe simplificações, padrão de batida da biblioteca, dedilhado (a tab como escrita, ou padrões p-i-m-a sobre a pegada) e as duas trocas a treinar. A saída só vira música se passar no `ChartValidator`:

- **Conservação:** a sequência de acordes da saída é igual à que o analisador léxico achou na fonte; nenhum acorde inventado ou sumido.
- Todo símbolo passa na gramática e resolve numa pegada da tabela; senão, simplificação visível (C7M → C).
- Capo e tom por busca determinística: mais acordes dominados, menos pestanas.
- Tocável: abertura ≤ 4 casas, até 4 dedos, casas 0 a 19, uma nota por corda; MIDI passa por um solucionador de digitação.
- Grade: compasso e BPM do arquivo; para cifra, toque de andamento (8 batidas no pedal) e um acorde por compasso, editável. Com áudio do jogador, Beat This! (MIT), fora do tempo real.
- Padrão de batida válido para o compasso; `obs` calculada por (pegada, corda).
- Até 2 reparos; depois o plano B determinístico (acordes como lidos, golpes para baixo nos tempos).

O jogador vê a prévia, toca uma passagem em modo espera e aperta "Adicionar à lista".

### 3.3 Cérebros

Trait `Brain` com adaptadores `openai-chat` (cobre Ollama e llama.cpp, localmente) e `anthropic`; `llama-embedded` depois. Adaptadores próprios e finos; a Rig é avaliada em um dia no M7 e só entra se reduzir código. Papéis: `helper`, `director`, `teacher`, `arranger`, `duel`. Sugestão: `helper` e `teacher` locais e pequenos; `arranger` de fronteira, uma chamada por música, em cache; `director` = regras. Falha, lentidão, saída inválida depois de 2 reparos ou orçamento estourado caem **sempre para regras**, com aviso; nunca para outra rota paga. Rota só fica disponível depois de uma chamada curta de teste.

## 4. Arquitetura

### 4.1 Crates (nomes definidos no início do projeto)

| Crate | Responsabilidade |
|---|---|
| `tonica-dsp` | filtros, decimação, STFT, fluxo, MPM, `presence`; `judge` (átomos, dicionário, NNLS, LLR, SPRT); `calib`; API C |
| `tonica-audio` | dispositivos `cpal`, `ClockMap`, latência, **mixer próprio**, decodificação |
| `tonica-theory` | notas, intervalos, gramática de acordes, tabela de pegadas (dados), capo, digitação, padrões de batida |
| `tonica-core` | `Chart`, importadores, `ChartValidator`, agenda de `Expect`, pontuação, domínio, diretor por regras, validadores de ferramenta |
| `tonica-agent` | `Brain`, adaptadores, ferramentas, intenção determinística, rastros JSONL, orçamento, bancada |
| `tonica-sim` | violão virtual, jogador virtual com erros injetados, cenários com asserções, tabelas FA/FR |
| `tonica-game` | Bevy: estados, pista, pauta, painel de cordas, efeitos, BRP |
| `tonica-cli` | julgar WAV contra chart, `record-script`, calibrar, bancadas, importar música |
| `tonica-ml` (novo, opcional) | `StringNet` e Beat This! em `tract-onnx` |

### 4.2 Threads

```
callback de entrada (tempo real): filtro, decimação, detector por quadro
   └─ rtrb ─▶ thread do juiz (prioridade alta, sem alocação, acorda a cada ~1 ms):
              segmentação, casamento com Expect, ajuste, SPRT
                 └─ rtrb ─▶ Bevy (drena no PreUpdate) ─▶ tonica-core ─▶ tela
callback de saída: mixer; comandos agendados por rtrb; carimbos para o ClockMap
auxiliares: ML, decodificação e esticamento, agente e STT, disco
```

Ajuste sobre a decisão inicial do detector: o detector por quadro fica no callback, como decidido; o ajuste de acorde (custo variável, até 500 µs) vai para a thread do juiz. Entra no ADR-002, com medição. O `tonica-core` é função pura do fluxo de `JudgeEvent`: toda sessão grava `events.jsonl` e pode ser reexecutada.

**Reprodução: mixer próprio** no fluxo de saída do `cpal`. O `bevy_seedling` 0.8 não acompanha o Bevy 0.20, e um motor só significa um relógio só. São 32 vozes pré-alocadas com início exato por amostra e uma voz de streaming. Andamento do áudio do jogador: pré-render (WSOLA) fora do tempo real, com troca no compasso. Firewheel direto é a alternativa se faltar grafo de efeitos.

### 4.3 Chart (`chart.json`, `v: 1`)

```jsonc
{ "v": 1, "id": "…", "title": "…", "origin": "paste|file|fetch|maestro|own",
  "tuning": [40,45,50,55,59,64], "capo": 0, "tpq": 480,
  "tempo_map": [{ "tick": 0, "bpm": 92 }], "time_sigs": [{ "tick": 0, "num": 4, "den": 4 }],
  "audio": null,
  "voicings": { "D": { "frets": [-1,0,0,2,3,2], "req": "XxMMMM", "fingers": [0,0,0,1,3,2] } },
  "patterns": { "pop8": { "grid": 8, "cells": ["D","-","D","U","-","U","D","U"] } },
  "sections": [{ "id": "verso1", "name": "Verso", "start": 0, "end": 15360 }],
  "tiers": [{ "tier": 2, "name": "Batida", "events": [
    { "tick": 0,   "dur": 240, "kind": "strum", "chord": "D", "dir": "D", "span": [2,5] },
    { "tick": 240, "dur": 240, "kind": "strum", "chord": "D", "dir": "U", "span": [3,5] },
    { "tick": 480, "dur": 240, "kind": "mute",  "dir": "D" },
    { "tick": 720, "dur": 480, "kind": "note",  "string": 2, "fret": 0, "finger": "p" },
    { "tick": 1200,"dur": 480, "kind": "rest" } ] }],
  "lyrics": [] }
```

`req` por corda: `M` Must, `m` May, `X` MustNot, `x` May no Aprendiz e MustNot a partir da Banda (regra padrão para corda grave que é nota do acorde). `span` = cordas que o golpe precisa atingir; as outras cordas da pegada viram `May`.

### 4.4 Versões

Da [pesquisa do ecossistema Rust](../pesquisa/ecossistema-rust-audio.md), a confirmar na fonte pelo ADR-001: toolchain ≥ 1.97.1; `bevy` =0.20.1; `bevy_brp_mcp` e `bevy_brp_extras` =0.23.0; `bevy_hanabi` =0.20.0; `bevy_egui` =0.43.1 (só painéis de desenvolvimento); `bevy_tweening` =0.17.0; `cpal` =0.18.2 com `realtime`; `rtrb` 0.4.1; `realfft` 3.5.1; `rubato` 5.1.0; `symphonia` 0.6.1 (MPL); `hound` 3.5.1; `criterion` 0.8.2; `assert_no_alloc` 1.1.2 (debug); `tract-onnx` 0.23.10 no lugar do `ort` (ainda rc, com DLLs). Não usar `bevy_seedling` nem `bevy_kira_audio`. Notação: desenho próprio no Bevy com a fonte Bravura (OFL).

### 4.5 Testes sem tela

- `tonica-sim` renderiza o WAV do cenário e alimenta o **mesmo código** do callback por um dispositivo falso com relógio virtual; compara os `JudgeEvent` com as asserções. Cenários de corda errada: `d_low_e_struck`, `c_as_am`, `g_missing_string1`, `f_dead_b_string`, `late_change_g_to_c`, `open_strings_only`, mais os de 1.11.
- Jogo: app Bevy com `MinimalPlugins` e os sistemas do núcleo, sem janela, tocado pelo jogador virtual. Agente: `FakeBrain` (válida, inválida, lenta, vazia).
- Aceite visual por capturas via BRP exige sessão gráfica; Bevy por SSH no Windows e a licença da crate `guitarpro` não estão verificados.

## 5. Tema e retorno

**Mundo.** Uma cidade de neon onde gatos tocam nos telhados. O braço do violão é o beco: cada casa tem uma janela que acende na cor do intervalo quando a nota é dominada. **O Maestro** é um gato produtor de visor holográfico e fones; fala curto e só entre rodadas. **A banda** entra com o progresso e passa a soar nas bases: baterista (Batida), baixista (camada Baixo), tecladista (acordes abertos), vocal de apoio (primeira música inteira). Palcos (cenário, não rigor): Garagem → Beco → Telhado → Clube → Arena. Dias tocados são luzes do letreiro, que só acendem. O público nunca vaia.

**Linguagem visual.** Ficam as 12 cores de `theory.js`. Formas: T ● vermelho; 3m ▼ e 3M ▲ (a ponta diz menor ou maior); 5J ◆ azul; 7m ⬡ violeta; as demais, quadrados pequenos. Uma corda = um tubo de neon; um golpe = uma seta grande. Na rodada, nenhum texto longo.

**Som.**

- Microfone aberto sem fone: sons do jogo com até 40 ms, não tonais e **acima de 6 kHz**; o juiz só olha até 5 kHz. O clique do metrônomo segue a mesma regra, porque cai exatamente no instante em que se espera o golpe.
- Com fone (conferido por um teste de vazamento): camada tonal completa. A banda "encaixa" quando o jogador acerta e recua para a bateria quando ele sofre: informação, não castigo.
- Acerto soa na hora; erro, baixo e depois do veredicto.

## 6. Plano de execução (PC)

"Barato" = implementação menor com contrato curto. "Cuidadoso" = implementação com revisão independente.

| Marco | Entrega | Pronto quando | Quem |
|---|---|---|---|
| M0 | toolchain no Windows, workspace, ADR-001 | [relatório M0 e M1](../relatorios/marcos-m0-m1.md) | barato |
| M1 | DSP em paridade (em andamento) | 5 cenários golden; custo por quadro medido | barato + revisão |
| M2 | áudio ao vivo: `cpal`, `ClockMap`, latência, mixer, afinador CLI | 10 min sem estouro nem alocação; σ do laço registrado; ADR-002 | cuidadoso (relógio); barato (CLI) |
| M3 | gravações reais: `record-script` e o roteiro de 10 min, no notebook e no USB | WAVs rotulados no conjunto de regressão | barato; o jogador grava |
| M4 | juiz v1 offline: `judge`, `calib`, violão virtual estendido, grade de cenários | portões sintéticos de 1.11 no CI; orçamento de 1.10 medido | **cuidadoso** (núcleo); barato (grade, tabelas) |
| M5 | juiz em dados reais: avaliador GuitarSet/IDMT e conjunto do jogador | relatório FA/FR/NO com limites; ADR-003 fixa limiares e diz o que cada nível liga | cuidadoso |
| M6 | fatia vertical no Bevy: Passagem de som + Estrada com chart próprio + painel de cordas + 3 níveis | o jogador virtual toca uma música inteira sem tela; `no_strum_at_all` dá `Miss` no jogo; o progresso persiste | barato (cenas) + cuidadoso (agenda de `Expect`) |
| M7 | ajuda: gramática, apertar para falar, ferramentas, chips; `Brain`, 2 adaptadores, `FakeBrain`, rastros | "mais devagar" vale no compasso seguinte sem LLM; lixo do `FakeBrain` nunca chega ao jogo; com "nenhum" tudo funciona | barato |
| M8 | Partitura: pauta + tab, espera, laço, andamento | o jogador virtual passa um estudo de 8 compassos nos dois regimes | barato (desenho) + cuidadoso (leiaute) |
| M9 | Montador: analisadores, `ChartValidator`, capo, toque de andamento, tela | cifra colada vira música jogável; conservação passa; injeção no título não vira ação | barato (analisadores) + cuidadoso (validador) |
| M10 | Caça-Notas, Raio-X, Trocas, Batida, Palco livre; domínio; banda e tema | cenários de cada modo verdes; aceite visual por revisor separado | barato |
| M11 | opcionais por medição: `StringNet` (ADR-004), câmera, Beat This!, áudio do jogador na Estrada, Duelo, macOS | cada um com tabela de antes e depois | cuidadoso |

O M3 vem cedo de propósito: o M4 precisa de áudio real para não se ajustar só ao sintético. Um Caça-Notas mínimo pode andar em paralelo, com implementação barata.

## 7. Riscos e decisões do usuário

**Riscos.**

1. As incógnitas de 1.12, sobretudo microfone de notebook e "aprimoramentos" do Windows. Mitigação: M3 cedo; talvez um microfone USB.
2. Cordas mascaradas por oitava podem ficar em "difícil de ouvir"; o jogo diz isso em vez de fingir que julga.
3. Calibração de 2,5 min é longa para este jogador; por isso é fase jogável, em duas partes.
4. `NaoOuvi` demais faz o juiz parecer surdo; há portão e autossuspeita.
5. Sem fone, o retorno sonoro fica pobre (só acima de 6 kHz); Bluetooth desliga o julgamento de ritmo.

**Decisões suas.**

1. **Buscar página de cifra:** manter desligado por padrão (recomendo) ou retirar? Os termos do Ultimate Guitar proíbem cópia; os do Cifra Club não foram lidos.
2. **Rascunho só pelo nome:** aceita acordes vindos da memória do modelo, marcados como rascunho?
3. **Bases:** autoriza baixar GuitarSet e IDMT-SMT-Guitar só para avaliação interna? A licença do IDMT pode ser não comercial.
4. **Copyleft:** este desenho não usa nenhum (Vosk small é Apache-2.0). O Maestro fica sem voz sintética ao vivo (sem eSpeak-NG)?
5. **GPU:** a `StringNet` treina em CPU. Beat This! e Demucs esperam o broker, ou há outra autorização?
6. **Equipamento:** violão de aço ou de náilon? Qual microfone? Fone com fio? Pedal USB (falar e contestar)?
7. **Nível inicial:** Aprendiz, com Banda depois da calibração completa (recomendo)?
8. **Cérebros:** as rotas do `brains.toml` continuam em aberto.
