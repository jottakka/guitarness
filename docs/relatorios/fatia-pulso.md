# Relatório da fatia Pulso

Data: 16/11/2026. Regras de desenho: [adendo 02](../design/adendo-02.md). Roteiro de validação: [roteiro da primeira sessão](../roteiro-primeira-sessao.md).

**Nada aqui vem de microfone ou alto-falante reais.** Toda a evidência é sintética (backend falso com relógio virtual, jogador virtual e violão simulado).

## 1. O que foi construído

- **Áudio:** anel de captura crua, posição de reprodução (playhead), relógio do motor com tratamento de estouros (xruns) e calibração do sistema e do jogador (`guitarness calibrate`).
- **Regras:** gráfico de fase, juiz do Pulso e tabela de portões com relatório, em `guitarness-core`.
- **Sessão:** fita gravada em disco, replay (`guitarness replay`), contestação de vereditos e bancada falsa, em `guitarness-session`.
- **Ferramentas:** `guitarness audio record`, `guitarness tune` e `guitarness calibrate`.
- **Jogo:** ponte de áudio, Passagem de som, tela do Pulso em 2D e 3D, resumo da rodada e contestação (Backspace).

O Pulso toca no `FakeBackend`, a fita grava, o replay é exato nos bancos de teste e o portão sintético fecha e abre. A revisão visual final e a verificação final em macOS e Windows ainda estão pendentes.

## 2. Testes

Contagens em compilação de release, salvo indicação.

| Área | Resultado |
|---|---|
| `guitarness-audio` | lib 66 (mais 1 de hardware ignorado), relógio 38, motor 13, backend falso 26, playhead 9, `no_alloc` 5 (depuração), captura 17, mixer 14, sons 3, calibração 48; `clippy` limpo com e sem `rt-audit` |
| `guitarness-core` | 87 testes passam, em depuração e release, com o teste sem alocação e o de contrato; 10 mutações pegas |
| `guitarness-session` | lib 9, sessão 27, fita 20, bancada 7, replay 4, portões sintéticos 1; `clippy` limpo |
| `guitarness-cli` | replay 8, calibração 7, áudio 9, afinador 6 |
| Jogo (`guitarness-game`) | ponte de áudio 7, Passagem de som 9, tela do Pulso 7, resumo e contestação 9 |
| Capturas sem janela | 2 de 17 PNGs diferem em até 1 nível em poucos pixels (suspeita: ruído de GPU) |

## 3. Precisão de tempo (medida em sintético)

| Medida | Valor |
|---|---|
| Detector, grade completa (6984 golpes): recall / extras / σ | 0,9970 / 0,09% / 0,95 ms |
| Detector, grade rápida no Mac: viés / p95 | +0,63 ms / 1,65 ms |
| Passagem curta: duração | 3,44 s (0,25 s de reinício do tap + 1,0 s de silêncio + 2,2 s da rodada) |
| Passagem curta: pior erro de `L_rt` em 24 salas | 0,007 ms |
| Calibrador contra juiz, mesmos 16 golpes (tendência de −30 ms) | −30,377 ms contra −30,375 ms (limite 1 ms) |
| Fronteira do estouro: degrau de relógio | −0,004 ms (antes da correção: +1,49 ms) |
| Playhead, p99 | ≤ 0,024 ms |
| Relógio com +120 ppm: p99 da linha / do carimbo | 0,011 ms / 0,12 ms |
| Subfluxo de saída de 20 ms | recuperado em 1 ms, em 0,06 s |
| Afinador (`guitarness tune`), cordas a −20, 0 e +15 cents | erro de 0,036 a 0,153 cents (limite 3) |

O limite de projeto de 1 ms da verdade em 20 s com +120 ppm é atendido pelos números do relógio e do playhead acima. O teste de aceite completo, com controle negativo (um relógio de quadros deve reprovar), não foi registrado.

## 4. Portões em fitas sintéticas

- **Replay:** 6 fitas de bancada com zero diferenças no nível `Events`. Uma adulteração proposital gera 1 diferença (controle negativo). No nível `Audio`, os ataques casam a menos de 0,01 ms. Fita incompleta fica fora e é nomeada.
- **Portão sintético:** 12 fitas, 636 golpes. `Attack` n = 656, `Timing` n = 652, 0 `false_wrong`, resultado Pass. O controle negativo (+40 ms de deslocamento) não passa. 25 repetições, 0 falhas.
- **Estado dos portões no padrão compilado:** `Attack`, `Timing` e `Tendency` estão em `Provisional`. Os outros sete tipos estão em `Locked`. Nenhum portão está `Open`: só fita real abre.
- **Leitura honesta:** com n ≈ 650 e 0 falsos, o limite de Wilson a 95% fica abaixo de 1%. Mas o oráculo (jogador virtual e `FakeBackend`) é nosso. Isso prova o encanamento, não o desempenho com guitarra real.

## 5. Windows

- `clippy -D warnings` limpo em `guitarness-audio` e `guitarness-cli`, incluindo o caminho `cfg(windows)`. Os testes em release passam, e o `no_alloc` em depuração passa 5/5. As linhas "memory allocation … failed" são os controles negativos esperados.
- O protótipo do jogo, antes das unidades do Pulso, compila em release com `clippy` limpo. Cada vista rodou 25 s em área de trabalho remota com renderizador por CPU (WARP), sem erro. O modo `--screenshot` falha no WARP; esse não é um alvo.
- **Não verificado:** o jogo da fatia Pulso no Windows e o comportamento do WASAPI com dispositivo real (ordem do erro de captura, `host_now_ns` contra carimbos).

## 6. Correções feitas durante a revisão

- Rodada curta da calibração aceitava `L_rt` com alias sob latência variável. Agora é rejeitada.
- A fronteira de estouros tardios ou em grupo estava errada. `note_xrun_at` devolve `LateNotice`, `UNDO_STAMPS` vale 64 e o anel do cpal tem 64 posições. Mudança de API: `ErrorFn` recebe `Option<XrunBoundary>`.
- A margem de fechamento do juiz subiu de 300 para 350 ms, apoiada por teste adversarial (pior atraso de golpe de 263,4 ms; com 300 ms houve 2 golpes tardios).
- Na fita: contestação oculta por UTF-8 truncado, completude que ignorava dano, perda de evento que não travava e falta de `fsync`. Todos corrigidos, com testes que falhavam antes.
- Golpe com `t_in` NaN virava falso "não ouvi", e o cabeçalho da fita guardava um só tempo por fita. Corrigido.

## 7. Capturas

Links relativos a este arquivo. Uma rodada final de correção visual ainda pode mudar os PNGs.

- Pulso 2D: [pulse-2d.png](../../crates/guitarness-game/shots/pulse-2d.png)
- Pulso 3D: [pulse-3d.png](../../crates/guitarness-game/shots/pulse-3d.png)
- Resumo: [pulse-summary.png](../../crates/guitarness-game/shots/pulse-summary.png)
- Detalhes: [pulse-details.png](../../crates/guitarness-game/shots/pulse-details.png)
- Contestar: [pulse-dispute.png](../../crates/guitarness-game/shots/pulse-dispute.png)
- Passagem de som, bipes: [soundcheck-beeps.png](../../crates/guitarness-game/shots/soundcheck-beeps.png)
- Passagem de som, palhetada: [soundcheck-strum.png](../../crates/guitarness-game/shots/soundcheck-strum.png)

Pontos para o olho humano: o "?" da contestação fica na linha inferior esquerda, não sobre o evento. O painel Detalhes mostra contagens (ouvidos de N, contestados), o que pede conferência contra a regra "sem contagem de erro". O desenho do plugue na Passagem de som é simples.

## 8. Limites abertos

1. **O modo ao vivo não foi testado em dispositivo real.** `--live`, o `CpalBackend` na fita e o `guitarness calibrate` no hardware passam só em testes sem dispositivo. O modo de carimbo funciona em teoria; o modo de linha precisa de um acessor de snapshot na sessão.
2. **Portões precisam de fitas reais.** Toda a evidência é sintética, e nenhum portão pode abrir. A pessoa responsável grava as fitas do roteiro.
3. **Estouro do anel cru em bancada rápida.** O banco de teste produz mais rápido que a thread da fita. O anel cru estoura, e a fita sai incompleta em cerca de 1 de 20 execuções. Os testes dormem por tick e repetem até 5 vezes. Em tempo real isso em princípio não ocorre, mas não foi medido. Se fitas reais saírem incompletas, revisar o formato da fita.
4. **Áudio cru só na primeira fita de cada época** do motor. Uma fita seguinte sem áudio não entra no replay de nível `Audio`.
5. **Tela fora do clique.** A latência de vídeo e de saída é desconhecida, pior com carimbo `Callback`. A pessoa responsável acerta `[` e `]` (±5 ms) e informa o valor.
6. **Limites do detector que não mudam:** martelada firme conta como golpe; golpe abafado pode ter pouca energia em 2–5 kHz; AGC do Windows; bloco de 480 quadros soma até 10 ms ao aviso.
7. **Rodadas rápida e completa de calibração** ainda aceitam por concordância, e as janelas podem alcançar uma sonda vizinha. Perfis feitos com limites frouxos são rejeitados na carga.
8. **Mapeamento de `judge_offset_ms` para `TimingRef`** foi testado só com 1 ms de tolerância, não com laço real.
9. **A fita só guarda o último cabeçalho de tempo por fita.** Isso é mitigado pelo tempo opcional na linha `anchor`. Fitas gravadas antes da correção não têm esse campo.
10. **`--render-out` do replay** gera um clique de 1 kHz por batida, não o som exato (a fita guarda a agenda, sem comandos do mixer).
11. **Caminho do perfil.** O padrão é `~/.guitarness/profiles.json`. Não foi confirmado que o jogo com `--data` usa o mesmo arquivo. O roteiro passa `--profile` aos comandos da CLI.
12. **Relógio do CoreAudio:** a fronteira do estouro é aproximada. O estouro de saída no WASAPI não é avisado.
13. **PlayNow** pode cancelar a Passagem de som, mas não há nova execução automática depois de `AudioLink::Back`.
14. **Direção e acordes não são julgados.** `Direction` e os tipos de acorde seguem `Locked`.
15. **Padrões em vigor:** fita desligada por padrão (opt-in com `--tape`), ausência de laço repete o passo de batida, Passagem de som com pictogramas e no máximo uma linha curta, subida de andamento só por oferta e portão do tempo `Provisional`.

## 9. Próximos passos

- Rodar a [primeira sessão com microfone e caixa de som reais](../roteiro-primeira-sessao.md) e gravar as fitas de referência.
- Medir falsa reprovação e falsa aprovação com essas fitas e decidir quais portões abrem.
- Fechar o ADR-002 com os testes em dispositivo real.
- Verificar o jogo da fatia Pulso no Windows.
