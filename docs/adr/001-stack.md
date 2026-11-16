# ADR-001: Stack do Tônica para PC

Data: 16/11/2026. Estado: aceito (revisar no portão do M2).

## Contexto

O jogo precisa ouvir um violão acústico por um microfone comum e julgar nota, acorde, corda e batida com latência baixa e sem engasgos. O primeiro alvo agora é **PC (Windows e macOS)**; celular fica para depois. O desenvolvimento roda em Windows 11 e em macOS (Apple silicon).

## Decisões

| Item | Escolha | Versão fixada | Licença | Motivo |
|---|---|---|---|---|
| Linguagem | Rust | toolchain **1.100.0** (`rust-toolchain.toml`) | MIT/Apache-2.0 | Tempo real sem coletor de lixo, SIMD, controle de alocação. Bevy 0.20 pede ≥ 1.97.1. |
| Engine | Bevy | **=0.20.1** (entra no M3) | MIT/Apache-2.0 | ECS em código, testável pela linha de comando. |
| Inspeção do jogo | bevy_brp_mcp / bevy_brp_extras | **=0.23.0** | MIT/Apache-2.0 | Já acompanha a Bevy 0.20. |
| Captura de áudio | cpal, feature `realtime` | **=0.18.2** | Apache-2.0 | Callback próprio; `realtime` registra a thread no MMCSS (Windows). |
| Fila entre threads | rtrb | 0.4.1 | MIT/Apache-2.0 | SPSC sem espera. |
| FFT | realfft (sobre rustfft) | **=3.5.1** | MIT | Autocorrelação por FFT: 7,4 µs por quadro. |
| Reamostragem | rubato | 5.1.0 (M2) | MIT/Apache-2.0 | Substitui a média de duas amostras. |
| Decodificação | symphonia | 0.6.1 (M6) | MPL-2.0 | Copyleft por arquivo, na lista permitida. |
| WAV | hound | **=3.5.1** | Apache-2.0 | Testes golden e CLI. |
| Benchmarks | criterion | **=0.8.2** | Apache-2.0/MIT | |
| Sem alocação | assert_no_alloc | **=1.1.2** | BSD-1-Clause | Sem manutenção desde 2021, mas funciona (controle negativo abaixo). |
| Partículas | bevy_hanabi | =0.20.0 (M3) | MIT/Apache-2.0 | |
| UI auxiliar | bevy_egui | =0.43.1 (M3) | MIT | Telas de configuração e painel da harness. |
| Reprodução | **a decidir no M2** por medição: (a) motor próprio no fluxo do cpal; (b) Firewheel 0.15 direto; (c) bevy_kira_audio 0.27.1 | — | MIT/Apache-2.0 | O `bevy_seedling` 0.8 só suporta Bevy 0.19, então sai da lista. |
| Inferência neural (opcional) | tract-onnx 0.23.10 (Rust puro) ou ort 2.0.0-rc.15 | — | MIT/Apache-2.0 | Só se um modelo vencer a bancada. O ort ainda é release candidate. |

Versões e datas conferidas na API do crates.io em 16/11/2026 (relatório em [ecossistema Rust de áudio](../pesquisa/ecossistema-rust-audio.md)).

## Consequências

- O áudio fica fora do mixer da Bevy. A Bevy só recebe eventos com carimbo de tempo do relógio de áudio.
- Perfil de release: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`.
- O Windows depende do MSVC (Build Tools).

## Alternativas registradas

- **Godot 4.7 + gdext**: continua como saída se a apresentação na Bevy não se sustentar. O núcleo não depende da engine.
- **Modo exclusivo WASAPI / ASIO**: o cpal 0.18 não mostra modo exclusivo no changelog (não verificado no código). Medir a latência real no M2 antes de decidir.
