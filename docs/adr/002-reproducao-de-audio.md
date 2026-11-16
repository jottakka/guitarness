# ADR-002: Reprodução de áudio — mixer próprio, Firewheel ou Kira

Data: 16/11/2026. Estado: **proposto**. Vira "aceito" quando os testes T2, T7, T10 e T11 passarem em dispositivo real e os anexos estiverem preenchidos.

## Contexto

O ADR-001 deixou a reprodução "a decidir no M2 por medição". As opções eram: (a) motor próprio no fluxo de saída do `cpal`; (b) Firewheel 0.15 direto; (c) `bevy_kira_audio` 0.27.1. O `bevy_seedling` 0.8 está fora: só aceita Bevy 0.19.

O jogo julga o instante de cada golpe. Por isso a reprodução tem requisitos que um jogo comum não tem:

- **R1. Início exato e informado.** A sonda de latência e o clique do metrônomo começam numa amostra escolhida. O juiz recebe de volta a amostra em que cada som começou de fato (`emits` do `measure_loopback`, batidas do `player_bias`).
- **R2. Um fluxo de saída, um relógio.** O tempo do chart é o contador de amostras da saída. Um segundo fluxo teria outra latência e outro relógio.
- **R3. Tempo real.** Sem alocação e sem trava no callback, provado por teste.
- **R4. Sem tela e sem placa.** O mesmo código de callback roda no dispositivo falso, no CI.
- **R5. Versões.** Bevy =0.20.1 e `cpal` =0.18.2 (ADR-001).
- **R6. Licença.** Sem GPL nem AGPL.
- **R7. O que o M2 precisa tocar.** Clique acima de 6 kHz, sonda, sons curtos agendados. Mais tarde (M6, M10): base da banda, áudio longo, troca de andamento no compasso.

## Opções

- **(a) Mixer próprio** dentro do callback de saída do `cpal`: 32 vozes pré-alocadas, agenda por amostra, gerador de metrônomo, eventos de volta por `rtrb`.
- **(b) Firewheel 0.15.0** (grafo de áudio; a integração com a Bevy é o `bevy_seedling`).
- **(c) Kira 0.12.6**, pelo `bevy_kira_audio` 0.27.1 ou direto.

## Evidência

Tudo lido em 16/11/2026. "Não verificado" quer dizer: a fonte não diz, e eu não rodei código.

| Critério | (a) Mixer próprio | (b) Firewheel 0.15.0 | (c) Kira 0.12.6 |
|---|---|---|---|
| R1 início exato | Por construção. Provado pelo T2 (erro de 0 amostra; saída idêntica em qualquer divisão de blocos). | Tem `clock::InstantSamples` e `EventInstant` ("When a particular audio event should occur"). A precisão não está documentada: o docs.rs marca "0% of the crate is documented". Não verificado. | Relógios e moduladores atualizam uma vez por bloco interno. Padrão: 128 amostras (≈ 2,7 ms a 48 kHz). Diminuir o bloco custa CPU. Se o som começa na amostra exata dentro do bloco: não verificado. |
| R1 início informado | `MixEvent::Started { at }` e `Beat { at }`. | Não encontrei. | Não encontrei. |
| R2 um fluxo | Sim. | O `firewheel-cpal` abre os próprios fluxos (`cpal ^0.18.2`). Existe `FirewheelProcessor::process(input, output, info)` público: daria para chamar do nosso callback. Não testado. | O backend `cpal` do Kira abre o próprio fluxo (`cpal ^0.18.1`). Existem o trait `Backend` e `Renderer::process(&mut [f32], num_channels)` públicos: daria para chamar do nosso callback. Não testado. O `bevy_kira_audio` cria o próprio gerenciador: seria um segundo fluxo. |
| R3 tempo real | Nosso, provado por T7 e T10. | README: "Properly respects realtime constraints (no mutexes!)". Tem um coletor para o que é solto na thread de áudio. Não afirma ausência de alocação. | Usa `rtrb`. Tem a feature opcional `assert_no_alloc`, pensada para quem desenvolve o Kira. |
| R4 dispositivo falso | Sim. | Só pelo `process` público (como em R2). | Só pelo `Renderer` público. Há um módulo `backend::mock`. |
| R5 versões | Sem dependência nova. | Publicado em 13/11/2026 (3 dias). O `bevy_seedling` 0.8 ainda pede `firewheel ^0.12`: a API andou três versões em três meses. O README diz que o projeto deve subir para dentro da Bevy. | `bevy_kira_audio` 0.27.1 pede `bevy ^0.20` e `kira ^0.12.1`. Estável. |
| R6 licença | — | MIT ou Apache-2.0. | MIT ou Apache-2.0. |
| R7 futuro | Falta escrever a voz de streaming (≈ 300 linhas). Não tem grafo de efeitos. | Grafo, nós prontos, entrada de microfone (`CpalInputConfig`), carga de arquivos. | Trilhas, efeitos, tweens, som em streaming, áudio espacial. |
| Custo no M2 | ≈ 450 linhas com testes. | Dependência nova e adaptador. O relato do início (R1) ainda ficaria por fazer. | Idem. |

## Decisão (recomendada)

1. **Mixer próprio (a) para tudo de que o juiz depende:** clique, sonda, contagem, retorno de acerto. Vale no M2 e fica. É a única opção em que R1 e R2 valem por construção e podem ser provados por teste nosso (T2). Nas outras duas, a precisão do início não está documentada e o início real não é informado.
2. **Camada musical (banda, áudio longo, efeitos): decidir depois do spike S1**, quando o M6 pedir. O spike embute um motor **dentro do nosso callback de saída**, como uma fonte a mais do mixer. Primeiro candidato: `kira::backend::Renderer` (estável, já na Bevy 0.20). Segundo: `FirewheelProcessor`. O S1 mede três coisas:
   - alocação em 10 min virtuais no dispositivo falso (zero);
   - custo do callback (p99 ≤ 10% do período);
   - erro de início contra o nosso relógio (esperado no Kira: até um bloco interno).
   Se a voz de streaming do mixer próprio cobrir o M6, nenhum motor entra.
3. **Proibido em qualquer caso:** abrir um segundo fluxo de saída; tocar som cronometrado pelo gerenciador do `bevy_kira_audio`; usar `bevy_seedling` enquanto ele não acompanhar a Bevy 0.20.

## Consequências

- O `guitarness-audio` ganha `mixer.rs` e `sounds.rs`. A Bevy só manda comandos (`MixCmd`) e lê eventos.
- Sem grafo de efeitos no M2. Com microfone aberto e sem fone, os sons do jogo ficam acima de 6 kHz (DESIGN §5); efeito sonoro rico só entra com fone.
- A voz de streaming e a troca de andamento por pré-render (WSOLA) ficam para o M6, fora do tempo real, como no DESIGN §4.2.
- O mixer mantém um ponto de extensão para uma fonte externa, usado só pelo S1.

## Decisão ligada: onde roda o detector

O DESIGN §4.2 pôs o detector por quadro no callback de entrada e pediu medição neste ADR. Mantido. Regra: se o p99 do callback de entrada passar de 10% do período, a `CaptureStage` vai para a thread de análise, sem mudar de código. O instante do ataque não muda com isso (é medido em amostras da entrada); só o aviso visual `Heard` ganha até ~1 ms.

## Fatos do `cpal` 0.18.2 que fecham pendências do ADR-001

- Não há modo exclusivo, modo cru nem `IAudioClient3` no backend WASAPI. O comentário do código diz "We always create voices in shared mode". O período é o do motor do Windows (≈ 10 ms).
- A feature `realtime` usa `audio_thread_priority` 0.35 (MPL-2.0) e, se falhar, `THREAD_PRIORITY_TIME_CRITICAL`.
- O ASIO fica fora: pede o SDK da Steinberg, cuja licença não conferi.

## Anexo A — medições do M2 (a preencher pela U13)

| Medida | Windows (WASAPI) | macOS (CoreAudio) |
|---|---|---|
| T2: erro de início do mixer (amostras) | | |
| T8: callback de entrada, média / p99 (µs) | | |
| T8: callback de saída, média / p99 (µs) | | |
| T10: estouros, saltos, descartes em 10 min | | |
| T10: pior callback (% do período) | | |
| T11: σ do laço (ms), inclinação (ms/min), tipo de laço | | |
| T12: faixa do `L_rt` em 10 reaberturas (ms) | | |
| Deriva entrada/saída (ppm) | | |

## Anexo B — spike S1 (a preencher se o M6 pedir)

| Motor | Alocações | p99 do callback | Erro de início | Decisão |
|---|---|---|---|---|
| `kira::backend::Renderer` | | | | |
| `FirewheelProcessor` | | | | |

## Fontes (lidas em 16/11/2026)

- Firewheel: https://docs.rs/firewheel/0.15.0/firewheel/ ; https://docs.rs/firewheel/0.15.0/firewheel/clock/index.html ; https://docs.rs/firewheel-graph/0.15.0/firewheel_graph/processor/struct.FirewheelProcessor.html ; https://docs.rs/firewheel-cpal/0.15.0/firewheel_cpal/ ; https://crates.io/api/v1/crates/firewheel/0.15.0/dependencies ; https://crates.io/api/v1/crates/firewheel-cpal/0.15.0/dependencies ; https://raw.githubusercontent.com/BillyDM/firewheel/main/README.md
- Kira: https://docs.rs/kira/0.12.6/kira/ ; https://docs.rs/kira/0.12.6/kira/struct.AudioManagerSettings.html ; https://docs.rs/kira/0.12.6/kira/backend/index.html ; https://docs.rs/kira/0.12.6/kira/backend/struct.Renderer.html ; https://docs.rs/kira/0.12.6/kira/enum.StartTime.html ; https://raw.githubusercontent.com/tesselode/kira/v0.12.6/crates/kira/src/backend/renderer.rs ; https://crates.io/api/v1/crates/kira/0.12.6/dependencies
- `bevy_kira_audio`: https://crates.io/api/v1/crates/bevy_kira_audio/0.27.1/dependencies
- `cpal`: https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/src/host/wasapi/device.rs ; https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/src/host/wasapi/stream.rs ; https://raw.githubusercontent.com/RustAudio/cpal/v0.18.2/Cargo.toml ; https://raw.githubusercontent.com/RustAudio/cpal/master/CHANGELOG.md
- `audio_thread_priority`: https://crates.io/api/v1/crates/audio_thread_priority/0.35.0
- Versões e datas dos crates: [ecossistema Rust de áudio](../pesquisa/ecossistema-rust-audio.md) (API do crates.io, 16/11/2026).

Limite desta pesquisa: li as páginas por uma ferramenta que resume. Não compilei nem rodei Firewheel ou Kira. O spike S1 existe para isso.

## Anexo C — um fluxo só, agora também no jogo (16/11/2026)

Este anexo registra o que mudou depois da fatia Pulso (relatório [fatia Pulso](../relatorios/fatia-pulso.md)). O texto da decisão acima não muda.

- **Um fluxo de saída, em todo o produto.** O jogo (`guitarness-game`) não liga mais `bevy_audio`, `rodio` nem `cpal` 0.17. As features do Bevy 0.20.1 são `2d`, `3d` e `ui`; o padrão sem `audio`. O motor `AudioEngine` do `guitarness-audio` (cpal 0.18.2) é a única pilha de áudio. Isso cumpre R2 e a proibição do item 3 da decisão (segundo fluxo de saída). Fonte: relato da G0 (a árvore resolvida não contém `rodio`, `bevy_audio` nem `cpal` 0.17.3; as linhas de lock listam esses nomes só como dependências opcionais do `bevy_internal`).
- **O relógio da tela vem do áudio.** `HostClock`, `Playhead` e `PlayClock` (A1) dão ao jogo o quadro de saída; o jogo não usa relógio de quadros para julgar. O juiz compara ataque e clique em quadros de áudio, nunca em tempo de vídeo (I2). Para a imagem fica `video_offset_ms` (teclas `[` e `]`, ±5 ms), gravado na fita.
- **Fronteira do estouro corrigida (R3, P23).** O `cpal_backend` grava a fronteira do `Xrun` num atômico; `ErrorFn` recebe `Option<XrunBoundary>`. Medido no `FakeBackend`: degrau do relógio no estouro −0,004 ms, contra +1,49 ms antes (relato da A1). WASAPI: erro de captura sai antes do callback do pacote afetado, conforme leitura do código do cpal 0.18.2; **não** verificado em dispositivo. CoreAudio: fronteira aproximada.
- **Anel cru e fita.** A capturadora ganhou um anel cru de 4 s (`RAW_SECONDS`) com marca `{in_first, det_first, frames}`, separado do tap de 2 s do calibrador. A fita grava a agenda do mixer, não um segundo WAV de saída; o mixer determinístico (T2) recria o som.
- **Calibração em duas partes.** `L_rt` é a latência do sistema; a tendência do jogador nunca é subtraída. A Passagem curta mede `L_rt` em 3,44 s virtuais, com erro de pior caso de 0,007 ms em 24 salas sintéticas (relato da A2).
- **O que continua aberto para fechar este ADR** (nada mudou na condição de "aceito"): os testes T2, T7, T10 e T11 em dispositivo real e o preenchimento do Anexo A. A fatia Pulso não abriu dispositivo. O ADR segue **proposto**. O jogo da fatia Pulso não foi verificado no Windows; o caminho `cfg(windows)` do `guitarness-audio` compila e passa clippy e testes lá.
- **Spike S1 (Anexo B):** não necessário nesta fatia. Nada no Pulso precisa de voz de streaming ou de motor externo.
