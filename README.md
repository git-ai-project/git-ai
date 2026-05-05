<div>
<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI por git-ai-project/git-ai" width="100" height="100" />

</div>
<div>
<h1 align="left"><b>git-ai</b></h1>
</div>
<p align="left">Rastrea el código generado por IA en tus repositorios</p>
<p align="left">
  <a href="https://discord.gg/XJStYvkb5U"><img alt="Discord" src="https://img.shields.io/badge/discord-unirse-5865F2?logo=discord&logoColor=white" /></a>
</p>

<video src="https://github.com/user-attachments/assets/68304ca6-b262-4638-9fb6-0a26f55c7986" muted loop controls autoplay></video>

## Inicio Rápido

#### Mac, Linux, Windows (WSL)

```bash
curl -sSL https://usegitai.com/install.sh | bash
```

#### Windows (sin WSL)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://usegitai.com/install.ps1 | iex"
```

🎊 ¡Eso es todo! **Sin configuración por repositorio.** Una vez instalado, Git AI funcionará de inmediato con cualquiera de estos **Agentes Compatibles**:

<img width="933" height="364" alt="seguimiento-de-código" src="https://github.com/user-attachments/assets/99ab05b1-97a9-4100-8ade-8ea8a227627b" />

### Documentación https://usegitai.com/docs
- [AI Blame](https://usegitai.com/docs/cli/ai-blame)
- [Almacenamiento de Prompts entre Agentes](https://usegitai.com/docs/cli/prompt-storage)
- [Referencia del CLI](https://usegitai.com/docs/cli/reference)
- [Configuración de Git AI para empresas](https://usegitai.com/docs/cli/configuration)

### Solo Instala y Haz Commit

Desarrolla como siempre. Solo escribe prompts, edita y haz commit. Git AI rastreará cada línea de código generado por IA y registrará el Agente de Codificación, el Modelo y el prompt que lo generó.

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/graph.jpg" width="400" />

#### ¿Cómo funciona?

Los Agentes de Codificación compatibles llaman a Git AI y marcan las líneas que insertan como generadas por IA.

Al hacer commit, Git AI guarda las atribuciones finales de IA en una Git Note. Estas notas alimentan AI-Blame, estadísticas de contribución de IA y más. El CLI se asegura de que estas notas se preserven a través de rebases, merges, squashes, cherry-picks, etc.

![Árbol Git](https://github.com/user-attachments/assets/edd20990-ec0b-4a53-afa4-89fa33de9541)

El formato de las notas se describe aquí en el [Estándar Git AI v3.0.0](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md)

## Objetivos del proyecto `git-ai`

🤖 **Rastrear código IA en un mundo Multi-Agente**. Dado que los desarrolladores eligen sus propias herramientas, los equipos de ingeniería necesitan una forma **agnóstica al proveedor** de rastrear el impacto de la IA en sus repositorios.

🎯 **Atribución precisa** desde Laptop → Pull Request → Merge. Claude Code, Cursor y Copilot no pueden rastrear el código después de generarlo—Git AI lo sigue a través de todo el flujo de trabajo.

🔄 **Soporte para flujos de trabajo git reales** asegurando que las anotaciones de autoría de IA sobrevivan a `merge --squash`, `rebase`, `reset`, `cherry-pick`, etc.

🔗 **Mantener el vínculo entre prompts y código** - hay contexto valioso y requisitos en los prompts del equipo—presérvalos junto al código.

🚀 **Nativo de Git + Rápido** - `git-ai` está construido sobre comandos de plomería de git. Impacto negligible incluso en repositorios grandes (&lt;100ms). Probado en [Chromium](https://github.com/chromium/chromium).

## Soporte de Agentes

`git-ai` configura automáticamente todos los hooks de agentes compatibles usando el comando `git-ai install-hooks`

| Agente/IDE                                                                                 | Autoría    | Prompts |
| ------------------------------------------------------------------------------------------ | ---------- | ------- |
| Claude Code                                                                                | ✅         | ✅      |
| OpenAI Codex &gt;0.99.0 (actualmente en versión alpha)                                     | ✅         | ✅      |
| Cursor &gt;1.7                                                                             | ✅         | ✅      |
| GitHub Copilot en VSCode vía Extensión                                                     | ✅         | ✅      |
| OpenCode                                                                                   | ✅         | ✅      |
| Google Gemini CLI                                                                          | ✅         | ✅      |
| Droid CLI (Factory AI)                                                                     | ✅         | ✅      |
| Continue CLI                                                                               | ✅         | ✅      |
| Atlassian RovoDev CLI                                                                      | ✅         | ✅      |
| GitHub Copilot en IDEs de Jetbrains (IntelliJ, etc.)                                       | ✅         | 🔄      |
| Jetbrains Junie                                                                            | ✅         | 🔄      |
| Amp (en progreso)                                                                          | 🔄         | 🔄      |
| AWS Kiro (en progreso)                                                                     | 🔄         | 🔄      |
| Continue VS Code/IntelliJ (en progreso)                                                    | 🔄         | 🔄      |
| Windsurf (en revisión)                                                                     | 🔄         | 🔄      |
| Augment Code                                                                               | 🔄         | 🔄      |
| Ona                                                                                        |            |         |
| Sourcegraph Cody                                                                           |            |         |
| Google Antigravity                                                                         |            |         |


> **¿Estás construyendo un Agente de Codificación?** [Agrega soporte para Git AI siguiendo esta guía](https://usegitai.com/docs/cli/add-your-agent)

## Instalación del Bot de Estadísticas (acceso anticipado)

Agrega datos de `git-ai` a nivel de PR, desarrollador, Repositorio y Organización:

- Desglose de autoría de IA para cada Pull Request
- Mide el % de código generado por IA a través de todo el SDLC
- Compara la tasa de aceptación del código escrito por cada Agente + Modelo
- Vida media del código IA (qué tan duradero es el código generado por IA)
> [Obtén acceso anticipado conversando con los mantenedores](https://calendly.com/acunniffe/meeting-with-git-ai-authors)

![alt](https://github.com/git-ai-project/git-ai/raw/main/assets/docs/dashboard.png)
