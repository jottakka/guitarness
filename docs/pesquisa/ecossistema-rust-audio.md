# Pesquisa: ecossistema Rust de motor e áudio

Data: 16/11/2026. Pesquisa assistida, com versões e datas lidas na API do crates.io. Este texto é pesquisa, não decisão.

| crate | versão | data | licença | notas |
|---|---|---|---|---|
| bevy | 0.20.1 (0.20.0 = 2026-10-08) | 2026-10-28 | MIT/Apache-2.0 | MSRV 1.97.1; guia de migração em bevy.org/learn/migration-guides/0-19-to-0-20/ |
| bevy_brp_mcp | 0.23.0 | 2026-10-09 | MIT/Apache-2.0 | bevy ^0.20 |
| bevy_brp_extras | 0.23.0 | 2026-10-09 | MIT/Apache-2.0 | bevy ^0.20, bevy_remote ^0.20 |
| bevy_seedling | 0.8.0 | 2026-08-09 | MIT/Apache-2.0 | NÃO é compatível com 0.20 (bevy ^0.19, firewheel ^0.12) |
| firewheel | 0.15.0 | 2026-11-13 | MIT/Apache-2.0 | o `firewheel-cpal` tem `CpalInputConfig`, com entrada de microfone no grafo (só li a configuração; não executei) |
| bevy_kira_audio | 0.27.1 | 2026-10-13 | MIT/Apache-2.0 | bevy ^0.20, kira ^0.12.1; só reprodução |
| kira | 0.12.6 | 2026-10-28 | MIT/Apache-2.0 | |
| bevy_hanabi | 0.20.0 | 2026-11-01 | MIT/Apache-2.0 | partículas na GPU, bevy ^0.20 |
| bevy_egui | 0.43.1 | 2026-10-10 | MIT | bevy ^0.20 |
| bevy_tweening | 0.17.0 | 2026-11-01 | MIT/Apache-2.0 | bevy ^0.20 |
| cpal | 0.18.2 | 2026-08-16 | Apache-2.0 | recurso `realtime` usa audio_thread_priority (MMCSS); não achei modo exclusivo do WASAPI (N/V); `BufferSize::Fixed` |
| rtrb | 0.4.1 | 2026-10-25 | MIT/Apache-2.0 | |
| assert_no_alloc | 1.1.2 | 2021-08-03 | BSD-1-Clause | sem manutenção, mas ainda funciona (alocador de depuração) |
| rustfft | 6.4.1 | 2025-09-18 | MIT/Apache-2.0 | |
| realfft | 3.5.1 | 2026-10-26 | MIT | |
| rubato | 5.1.0 | 2026-11-12 | MIT/Apache-2.0 | |
| symphonia | 0.6.1 | 2026-08-13 | MPL-2.0 | copyleft de arquivo (aceitável) |
| hound | 3.5.1 | 2023-09-25 | Apache-2.0 | |
| criterion | 0.8.2 | 2026-02-04 | Apache-2.0/MIT | |
| divan | 0.1.21 | 2025-04-10 | MIT/Apache-2.0 | |
| ort | 2.0.0-rc.15 | 2026-10-19 | MIT/Apache-2.0 | ainda candidato a versão (rc); cuda, directml e coreml; recomenda-se `load-dynamic` |
| candle-core | 0.11.1 | 2026-10-31 | MIT/Apache-2.0 | |
| burn | 0.22.1 | 2026-10-20 | MIT/Apache-2.0 | |
| tract-onnx | 0.23.10 | 2026-11-05 | MIT/Apache-2.0 | ONNX em Rust puro, sem DLLs |
| audio_thread_priority | 0.38.0 | 2026-09-17 | MPL-2.0 | MMCSS e restrição de tempo do Mach (N/V) |

## Armadilhas

- O bevy_seedling parou no 0.19. Usar bevy_kira_audio 0.27.1, o cpal direto ou o firewheel direto.
- Entrada de microfone fora do Bevy: callback do cpal 0.18 mais rtrb até o Bevy.
- O WASAPI compartilhado tem período de motor de cerca de 10 ms; o modo exclusivo não foi achado no cpal; o recurso ASIO é opcional (exige o SDK do ASIO).
- Recurso `realtime` do cpal para o MMCSS.
- Windows com MSVC: carga de trabalho "Desktop C++" e Windows SDK; `dynamic_linking` só em desenvolvimento; LLD com `linker = "rust-lld.exe"`.
- macOS no Bevy 0.20: janelas novas tomam o foco, a menos que `Window::focused=false`.
- Fixar a toolchain em 1.97.1 ou mais nova.
- ort em rc: `load-dynamic`, ou `download-binaries` mais `copy-dylibs`; DirectML (Windows) e CoreML (macOS). O tract-onnx tem risco menor.
- Bevy sem janela por SSH no Windows não foi verificado; o wgpu precisa de um adaptador (DX12 ou WARP). Testes de lógica e DSP funcionam por SSH.

## Versões fixadas recomendadas

bevy =0.20.1; bevy_brp_mcp e bevy_brp_extras =0.23.0; bevy_hanabi =0.20.0; bevy_egui =0.43.1; bevy_tweening =0.17.0; bevy_kira_audio =0.27.1; cpal =0.18.2 (realtime); rtrb 0.4.1; rustfft 6.4.1; realfft 3.5.1; rubato 5.1.0; symphonia 0.6.1; hound 3.5.1; criterion 0.8.2 (dev); assert_no_alloc 1.1.2 (depuração); ort 2.0.0-rc.15 (opcional) ou tract-onnx 0.23.10.
