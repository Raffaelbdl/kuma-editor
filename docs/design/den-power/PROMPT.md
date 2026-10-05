# First prompt for Claude Code

Paste this into Claude Code, started at the root of `kuma-editor`, after copying this folder to `docs/design/den-power/`:

---

Read `docs/decisions.md`, then `CLAUDE.md`, then `docs/design/den-power/README.md`, and open `docs/design/den-power/Kuma Den - Power.dc.html` in a browser for the visuals (artboards 2a, 2b, 2c).

We're redesigning the editor UI in `ui/` into two modes over one set of panels: **Power** (2a) and **Den** (2b). The README is the spec. Before you write code, I want a plan, not an implementation:

1. **Map the panels.** For each module in 2c (timeline, bin, inspector, agent, history, pins, render queue), say which existing file in `ui/src/panels/` it comes from, and what has to change so it can mount in any container (docked in power, floating in den, its own window). Agent, history and pins are new.
2. **Panel host.** Propose a small `PanelHost` (docked / floating / window) and how panel letters become `command()`s (`B I T A H M R`, `P` for a pin) so ⌘K, the shell menu and the keys all reach them. Popping out uses `window.open('?panel=<id>')` in a browser, and a frameless BrowserWindow from `app/` via `desktop()` in the shell. The UI must still work without the shell.
3. **Daemon gaps.** Check what the daemon already exposes. The agent feed needs MCP tool calls broadcast on the WS (tool, short args, revision before/after, `sample_frames` times + rect, lint result). History needs revisions with author (ui/mcp), summary, time and refused patches, read from the undo stack. Pins should be the existing markers. List the exact Rust changes in `kuma-daemon` / `kuma-mcp`, and keep every rule in `CLAUDE.md` (all state in the daemon, thin clients, no client-side model of the compiler).
4. **Tokens.** Add the new values to `themes.css` as tokens on the `kuma` theme (and give `tokyo-night` equivalents so its test still passes). `style.css` names tokens only, never a colour. Fonts: JetBrains Mono, IBM Plex Sans and Silkscreen. Bundle them locally; no CDN at runtime.
5. **Order of work.** Propose small PRs, each one shippable. Suggested: (a) tokens + type + top bar/status line restyle of the current layout; (b) rail + ⌘K; (c) PanelHost + den mode with the agent panel; (d) daemon activity feed + viewfinder corners + bear states; (e) history + pins panels; (f) pop-out windows.

Don't change the project file format. Keep the editor page identical between browser and shell, except behind `desktop()`. Write the plan to `docs/design/den-power/PLAN.md` and stop so I can review it.
