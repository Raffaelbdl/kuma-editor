# Handoff: kuma — Den mode + Power mode

## Overview
A redesign of the kuma editor UI (`ui/`) around the agent that edits alongside you over MCP. Two modes over the same panels:

- **Power** (`#2a`): the full editor, restyled as an IDE / terminal. A rail of letters, ⌘K, a status line like vim's, and a history at the bottom of the inspector.
- **Den** (`#2b`): the frame is full-bleed and shows what the agent did and where it looked. Panels are summoned on demand and float. They can be torn off into windows on a second screen.
- **Panel modules** (`#2c`): every panel at its floating size. In both modes they are the **same modules**; only the container changes (docked, floating or window).

The agent stays **external** (Claude Code etc. over MCP). The editor has no chat. The user's only way to write to the agent is a **pin**: a comment on a frame, stored as a timeline marker that the agent reads.

## About the design files
`Kuma Den - Power.dc.html` is a **design reference built in HTML**. It's a static mock with inline styles, not production code. Recreate it inside the existing `ui/` app (Vite + TypeScript, no framework, `el()`/`button()` helpers from `dom.ts`, the commands in `commands.ts`, the store in `store.ts`), following its patterns. Open the file in a browser: it's a pan/zoom canvas with the three artboards.

## Fidelity
**High fidelity** for colours, type, layout and the visual language. Copy and sample data (revisions r55–r59, track names, times) are illustrative.

## Screens

### 2a — Power (1440×900 reference)
CSS grid, top to bottom: **top bar 44px** · **main 532px** · **timeline 300px** · **status line 24px**.
- **Top bar** (`#0e0f12`, bottom border `#1a1c22`). Left to right: pixel bear 22×18 (`image-rendering:pixelated`), the wordmark "kuma" in Silkscreen 14px, then editor tabs (one per open sequence / workbench entry). The active tab is `#1a1c22` with a 2px top border in `#ff4f00`. Then a centred ⌘K field 320×28 (border `#2e323c`), the den/power toggle (Silkscreen 10px, active segment `#e6e7eb` bg / `#0b0c0f` text), **render** (`#ff4f00` bg, `#0b0c0f` bold text), and **queue** (outlined, "1 running · 27 done"; it opens the render queue, as the job status does today).
- **Rail** (44px wide). Letter tiles 30×30 in Silkscreen 12px: `b` bin · `p` blueprints · `f` fonts · `m` pins/markers · `h` history · `r` render queue. Active: `#ff4f00` bg / `#0b0c0f` text. Hover tooltip: "history · H" (`#e6e7eb` bg). `m` has a yellow dot when a pin is open.
- **Bin** (232px). Header "bin" in Silkscreen 10px `#9aa0ad`, with + and import. A "/ filter" field. A tree in JetBrains Mono 12px with 8×8 **kind squares** instead of text badges. The open sequence's row is `#1a1c22` with text in `#ffd166`.
- **Viewer** (flex 1, bg `#08090b`). Frame 820×461 with a 1px `#23262e` ring. The selection box is a 1px `#ffd166` outline with 7×7 square handles. Transport row in 12px: `|◂ ▶ 00:47.00 / 01:47.08 · ½ res · fit · 1920×1080 ⇄`.
- **Inspector** (288px). Section labels in Silkscreen 10px `#9aa0ad`. Number fields 24px tall with a 1px `#2e323c` border and a tag cell on the left. Axis tags are coloured X `#d9576b` and Y `#59c07a` at 14% tint (the existing `.num.axis-*`). Sections: clip header · placement (t, d) · transform (position, scale, opacity) · vars from `overlay.json` · **history · this clip** pinned to the bottom (rows: rev, change, author kuma/you). **Changed fields are not highlighted**: history replaces that.
- **Timeline** (`#0e0f12`). Ruler 24px (10px mono `#5c616d`, a tick every 10 s). Marker flags on the ruler: `#2a2615` bg with a 2px `#ffd166` left border. Tracks 36px tall with a 150px head (a 3px left border in the kind colour, a short kind code `ov`/`v1`/`a1`, a name, and a toggle). Clips: radius 2px, mono 10.5px, kind colours. Selected: a 2px `#ffd166` outline. Playhead: 2px `#ff4f00` with a 10×10 square head. **No agent lane.**
- **Status line** (24px, `#ff4f00` bg, `#0b0c0f` text, 11px). Segments: `power` (inverted, Silkscreen) · `r59` · selection count · playhead … `1920×1080 · 30 fps` · `● claude-code · reading pin 2` · `27 jobs done`.

### 2b — Den (1440×900 reference)
- The frame fills the top 1440×810. A gradient at the top for legibility (`rgba(0,0,0,.55)` → transparent, 110px).
- **HUD, top-left:** bear (its state image), a "den" chip (`#ff4f00`, Silkscreen 13px), sequence name, `r59 · kuma looking`. **Top-right:** "power ↗" (it opens power at the playhead).
- **Viewfinder:** orange corner brackets (4px, 28px arms, `#ff4f00`) around the rect of the agent's last `sample_frames` call, if it passed one; otherwise around the full frame.
- **Caption:** the summary of the last action, centred at y≈700, `rgba(0,0,0,.82)` box, revision in mono 14px `#ff7a3d` plus IBM Plex Sans 500 21px.
- **Bottom rail (90px, `#0b0c0f`):** play, time, and a timeline track with **revision markers placed at the time they edited** (9px orange squares labelled r57/r58; the latest is 11px with a 3px glow ring). Pins are 9px yellow dots, playhead 2px `#e6e7eb`. On the right: **summon keys** `b i t a h m r` (30×30 tiles, outlined when off; on = `#e6e7eb` bg, or `#ff4f00` for `a`) and a **pin** button (yellow outline).
- **Floating panels:** 300px wide, `rgba(14,15,18,.94)`, 1px `#2e323c`, shadow `0 14px 40px rgba(0,0,0,.6)`. 28px title bar (`#1a1c22`) holding a 16×16 letter tile, the title in Silkscreen 10px, ⧉ (pop out to a window) and ×. Dragging a panel against a screen edge shows a dashed orange edge (4px) and the hint "drag past the edge → window on screen 2".

### 2c — Panel modules
`t` timeline (a wide strip; dock it to the bottom and it becomes power's timeline) · `b` bin · `i` inspector (ends with that clip's history) · `a` agent (current state and the last 5 tool calls, read-only) · `h` history (one undo stack; filters all / kuma / you; ⟲ restores a revision; refused patches listed, greyed) · `m` pins (open / being read / done in rN; "+ pin at playhead", key P) · `r` render queue (progress, queued including agent-requested, done with path; "render master").

## Interactions & behaviour
- **The den/power toggle** keeps the playhead and the selection. "power ↗" in den opens power at the playhead.
- **Letters are commands**, registered with `command()` so ⌘K, the menus and the keys all reach them: `B I T A H M R` toggle panels (in den they summon a floating panel; in power they focus the rail section or panel). `P` adds a pin at the playhead, then focuses its text.
- **Floating:** drag by the title bar; it snaps docked within 24px of an edge. ⧉, or dragging past the window edge, opens the panel in its own window. In a browser that's `window.open(?panel=<id>)`; in the shell it's a new BrowserWindow with the same frameless title bar. Remember the layout per mode, in localStorage (view state, never the document).
- **Bear states:** idle `bear-sleep.png` (no agent activity for over 60 s) · editing `bear.png` (apply_edits in flight or just landed) · looking `bear-look.png` (sample_frames running). The cursor after the status text blinks with `steps(1)` at 1 s.
- **Pins** are the existing timeline markers with a comment. "Done in rN" means a later revision whose agent edit references the pin, or that the user ticked by hand. To be decided.

## State / data (verify against the daemon)
- **Agent activity feed:** the daemon has to broadcast MCP tool calls (name, args summary, revision before → after, `sample_frames` times/rect, lint result) on the existing WS. Today the UI only sees the resulting patches.
- **History:** revisions with author (`ui` vs `mcp`), op count, summary and timestamp, including refused stale patches. Take it from the daemon's undo stack, never from a client-side model.
- **UI state:** mode, summoned panels, positions/docking, popped-out windows. All of it is view state.

## Design tokens (kuma theme; add to `themes.css`, don't hardcode)
Backgrounds: `#08090b` stage · `#0b0c0f` app · `#0e0f12` bars/timeline · `#121317` · `#1a1c22` raised/selected · `#23262e` · lines `#2e323c` / subtle `#1a1c22`.
Text: `#e6e7eb` · `#c4c8d2` body-secondary · `#9aa0ad` labels · `#5c616d` dim.
Accent `#ff4f00` (on-accent `#0b0c0f`) · accent text `#ff7a3d` · selection/pins `#ffd166` (flag bg `#2a2615`) · ok `#3f8a5c` · diff + `#59c07a` / − `#d9576b`.
Kinds (unchanged): overlay `#b0532a` · video `#3b6ea5` · audio `#3f8a5c` · sequence `#3b5c7a` · folder `#6b5a2a` · colour `#555`.
Type: **JetBrains Mono** 400/500/700 for all UI chrome (10–14px) · **IBM Plex Sans** 400–600 only for summary sentences · **Silkscreen** only for section labels, letters and the wordmark (10–14px). Radii: 0–4px (square is the house style). Shadows: floating panels only.

## Assets
- `assets/bear.png`, `bear-look.png`, `bear-sleep.png`: 12×10 pixel bear. Always scale it by whole numbers with `image-rendering: pixelated`.
- `assets/frame-twist.png`: a placeholder frame cropped from `docs/media/kuma-editor.gif`. In the app this is the live preview.
- No new icons. The glyphs used are ⧉ × ▶ ▾ ⟲ ↗ (or the existing `icons.ts` paths).

## Files
- `Kuma Den - Power.dc.html`: the reference (open it in a browser; it needs `support.js` next to it).
- `PROMPT.md`: the first prompt to give Claude Code.
