# Documentação

Toda a documentação está em português do Brasil. O código-fonte e seus comentários de API estão em inglês.

O projeto se chamava Tônica antes de virar Guitarness. Documentos escritos em 16/11/2026 refletem o estado daquela data.

## Comece por aqui

- [Visão geral da arquitetura](arquitetura/visao-geral.md): crates, fluxo de áudio até o jogo, aprendiz, contrato e verificador, tempo real e portões.
- [Roteiro da primeira sessão com microfone real](roteiro-primeira-sessao.md): como gravar as fitas que abrem os portões.

## Design

| Documento | Conteúdo |
|---|---|
| [Design do jogo](design/design-do-jogo.md) | Princípios, juiz estrito, perfis, marcos e perguntas em aberto. |
| [Adendo 01](design/adendo-01.md) | Vistas 3D e 2D, tipos de golpe, modo Pulso e estado medido do detector. |
| [Adendo 02](design/adendo-02.md) | Tempo, direção, portões e fita. Vale sobre os textos anteriores em caso de conflito. |

## Decisões de arquitetura (ADR)

| Documento | Estado |
|---|---|
| [ADR-001: stack](adr/001-stack.md) | Rust, Bevy e `cpal`. |
| [ADR-002: reprodução de áudio](adr/002-reproducao-de-audio.md) | Proposto. Depende de testes em dispositivo real. |

O design cita os ADR-003 (limiares do juiz) e ADR-004 (rede por corda). Eles ainda não foram escritos.

## Relatórios

| Documento | Conteúdo |
|---|---|
| [Marcos M0 e M1](relatorios/marcos-m0-m1.md) | Paridade do DSP com a demo web, custo por quadro e ausência de alocação. |
| [Fatia Pulso](relatorios/fatia-pulso.md) | Testes, precisão de tempo, portões sintéticos e limites abertos. |
| [Rodada 2](relatorios/rodada-2.md) | Aprendiz, verificador de acordes, contrato por evento, `guitarness verify`, comportamento da testemunha e limites. |

## Pesquisa

Pesquisas de apoio. Elas não são decisões; o design manda.

| Documento | Tema |
|---|---|
| [Game design](pesquisa/game-design.md) | Ideias que funcionaram em outros jogos e como aplicar. |
| [Ensino, métricas e perfis](pesquisa/ensino-metricas-perfis.md) | Aprendizagem motora, histórico e perfis de jogador. |
| [Modelos de detecção](pesquisa/modelos-deteccao.md) | Modelos já treinados e conjuntos de dados, com licenças. |
| [Detecção de acorde, corda e batida](pesquisa/deteccao-acordes-cordas.md) | Estado da arte e abordagem recomendada. |
| [Fontes de cifras](pesquisa/fontes-de-cifras.md) | Origem, leitura e notação de cifras e tablaturas. |
| [Ecossistema Rust de áudio](pesquisa/ecossistema-rust-audio.md) | Versões e licenças de crates. |

## Arte

[Arte conceitual minimalista](arte/minimal/README.md): cinco imagens geradas por IA, com os prompts e a proveniência.

## Demo legada

[`legacy/LEIA-ME.md`](../legacy/LEIA-ME.md) descreve a demo web em JavaScript que originou o detector.
