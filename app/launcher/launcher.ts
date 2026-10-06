// The launcher page. It talks to the main process through `window.launcher` (see
// `preload/launcher.cjs`) and never touches the filesystem or a daemon itself.

interface Recent {
  path: string;
  name: string;
  openedAt: number;
  width?: number;
  height?: number;
  fps?: string;
  missing?: boolean;
}

interface AppInfo {
  version: string;
  kuma: { bin: string; repo: string; version: string } | null;
  error: string | null;
  warnings: string[];
  /** Set when no template library is installed and the checkout's starter set is used. */
  libraryFallback: string | null;
  defaultParent: string;
}

interface NewProject {
  parentDir: string;
  name: string;
  width: number;
  height: number;
  fps: string;
}

interface LauncherBridge {
  info(): Promise<AppInfo>;
  recents(): Promise<Recent[]>;
  forget(file: string): Promise<Recent[]>;
  clearRecents(): Promise<Recent[]>;
  pickProject(): Promise<string | null>;
  pickParent(): Promise<string | null>;
  previewPath(parentDir: string, name: string): Promise<string>;
  create(spec: NewProject): Promise<string>;
  open(file: string): Promise<boolean>;
  pathForFile(file: File): string;
  onCommand(fn: (name: string) => void): () => void;
  onRecentsChanged(fn: () => void): () => void;
}

declare const launcher: LauncherBridge;

// ---- tiny DOM helpers (same contract as the editor's: textContent, never innerHTML) ----

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function button(label: string, onClick: () => void, className = ""): HTMLButtonElement {
  const b = el("button", className);
  b.textContent = label;
  b.onclick = onClick;
  return b;
}

/**
 * A path short enough for one line, keeping both ends: the tail names the project and the
 * head says where it lives. Done here rather than with `text-overflow`, which can only
 * drop one end, or `direction: rtl`, which reorders the leading separator.
 */
function shortPath(p: string, max = 68): string {
  const home = info?.defaultParent;
  const short = home && p.startsWith(home + "/") ? "~" + p.slice(home.length) : p;
  if (short.length <= max) return short;
  const keepTail = Math.max(24, Math.floor(max * 0.6));
  return `${short.slice(0, max - keepTail - 1)}…${short.slice(-keepTail)}`;
}

/** "just now", "3 hours ago", "12 Mar" — enough to tell one recent project from another. */
function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 90) return "just now";
  const units: [number, string][] = [[60, "minute"], [3600, "hour"], [86400, "day"]];
  for (let i = units.length - 1; i >= 0; i--) {
    const [size, name] = units[i];
    const n = Math.floor(s / size);
    if (n >= 1 && s < 86400 * 7) return `${n} ${name}${n === 1 ? "" : "s"} ago`;
  }
  return new Date(ms).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

// ---- state ----

const main = document.getElementById("main")!;
const actions = document.getElementById("actions")!;
const statusLine = document.getElementById("statusline")!;
const dropHint = document.getElementById("drop-hint")!;

let info: AppInfo;
let recents: Recent[] = [];
let filter = "";
let view: "recents" | "new" = "recents";

const MOD = navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl ";

/** A page: the one column the launcher reads in, with what is wrong with kuma on top. */
function page(): HTMLElement {
  const p = el("div", "page");
  const notes = notices();
  if (notes) p.appendChild(notes);
  main.replaceChildren(p);
  return p;
}

// ---- the chrome: the top bar's two ways in, and the status line ----

function renderChrome() {
  const ready = !!info.kuma;
  const key = (label: string, k: string) => {
    const b = el("button");
    b.append(label, el("kbd", "", `${MOD}${k}`));
    return b;
  };
  const openBtn = key("open…", "O");
  openBtn.title = "Open a .kuma.json you already have — or drop one on this window";
  openBtn.onclick = () => void openDialog();
  const newBtn = key("new project", "N");
  newBtn.className = "primary";
  newBtn.title = "Create a folder with a .kuma.json in it";
  newBtn.onclick = () => showNew();
  openBtn.disabled = newBtn.disabled = !ready;
  actions.replaceChildren(openBtn, newBtn);

  // Which kuma this app will run, and from where: the usual cause of a project the app
  // cannot open is a `kuma` from somewhere other than the checkout you think you are running.
  statusLine.replaceChildren(el("span", "mode", "launcher"), el("span", "", `app ${info.version}`));
  if (info.kuma) {
    statusLine.appendChild(el("span", "", info.kuma.version));
    const where = el("span", "grow", shortPath(info.kuma.bin, 80));
    where.title = info.kuma.bin;
    statusLine.appendChild(where);
  } else {
    statusLine.appendChild(el("span", "bad grow", "kuma binary not found"));
  }
  statusLine.appendChild(el("span", "last", "drop a .kuma.json to open it"));
}

/** What is wrong, or worth knowing, about the kuma this app found; null when nothing is. */
function notices(): HTMLElement | null {
  const box = el("div", "notices");
  if (!info.kuma) box.appendChild(el("div", "notice bad", info.error ?? "kuma binary not found"));
  else {
    if (info.libraryFallback) {
      const note = el("div", "notice");
      note.append("No template library is installed, so the checkout's starter set is used. ", el("code", "", "kuma library"), " installs it properly.");
      note.title = info.libraryFallback;
      box.appendChild(note);
    }
    for (const w of info.warnings) box.appendChild(el("div", "notice", w));
  }
  return box.childElementCount ? box : null;
}

// ---- recents ----

function showRecents() {
  view = "recents";
  const p = page();

  const head = el("div", "head");
  const count = el("span", "count", recents.length ? String(recents.length) : "");
  const filterBox = el("label", "filter");
  const search = el("input");
  search.type = "search";
  search.placeholder = "filter by name or path";
  search.spellcheck = false;
  search.value = filter;
  search.oninput = () => {
    filter = search.value;
    renderList(list);
  };
  filterBox.append(el("span", "", "/"), search);
  head.append(el("h1", "", "projects"), count, el("span", "spacer"));
  if (recents.length) {
    head.appendChild(filterBox);
    const clear = button("clear", async () => {
      recents = await launcher.clearRecents();
      showRecents();
    }, "ghost");
    clear.title = "Forget every project here — the files stay where they are";
    head.appendChild(clear);
  }
  p.appendChild(head);

  const list = el("div", "list");
  p.appendChild(list);
  renderList(list);
  // The filter is the only thing worth typing into here.
  if (recents.length > 6) search.focus();
}

/** The frame's shape, to scale inside a 36px box, so a vertical project looks like one. */
function shape(r: Recent): HTMLElement {
  const box = el("div", "shape");
  const frame = el("i");
  const w = r.width && r.height ? r.width : 16;
  const h = r.width && r.height ? r.height : 9;
  const k = 30 / Math.max(w, h);
  frame.style.width = `${Math.max(6, Math.round(w * k))}px`;
  frame.style.height = `${Math.max(6, Math.round(h * k))}px`;
  box.appendChild(frame);
  return box;
}

function renderList(list: HTMLElement) {
  list.replaceChildren();
  const q = filter.trim().toLowerCase();
  const shown = recents.filter((r) => !q || r.name.toLowerCase().includes(q) || r.path.toLowerCase().includes(q));

  if (!shown.length) {
    // The list's own frame is for rows; the empty state draws its own.
    list.className = "";
    const empty = el("div", "empty");
    if (recents.length) {
      empty.append(el("div", "what", "nothing matches"), el("div", "how", "No project here has that in its name or its path."));
    } else {
      const sleeping = el("img", "bear");
      sleeping.src = "bear-sleep.png";
      sleeping.alt = "";
      sleeping.width = 72;
      sleeping.height = 60;
      empty.append(sleeping, el("div", "what", "no projects yet"), el("div", "how", "Start a new one, or open a .kuma.json you already have — you can drop it on this window too."));
    }
    list.appendChild(empty);
    return;
  }
  list.className = "list";

  for (const r of shown) {
    const row = el("div", `recent${r.missing ? " missing" : ""}`);
    const who = el("div", "who");
    const name = el("div", "name");
    name.appendChild(el("span", "", r.name));
    if (r.missing) name.appendChild(el("span", "tag", "missing"));
    const pathLine = el("div", "path", shortPath(r.path));
    pathLine.title = r.path;
    who.append(name, pathLine);

    const meta = el("div", "meta");
    meta.append(
      el("div", "", [r.width && r.height ? `${r.width}×${r.height}` : "", r.fps ? `${r.fps} fps` : ""].filter(Boolean).join(" · ")),
      el("div", "when", ago(r.openedAt)),
    );

    // Forgetting keeps the project on disk; it only drops it from this list.
    const forget = button("forget", () => {}, "forget");
    forget.title = "Drop it from this list — the project stays on disk";
    forget.onclick = async (e) => {
      e.stopPropagation();
      recents = await launcher.forget(r.path);
      showRecents();
    };

    row.append(shape(r), who, meta, forget);
    if (!r.missing) {
      row.tabIndex = 0;
      row.onclick = () => void open(r.path);
      row.onkeydown = (e) => {
        if (e.key === "Enter" && e.target === row) void open(r.path);
      };
    }
    list.appendChild(row);
  }
}

// ---- new project ----

const SIZES: { label: string; w: number; h: number }[] = [
  { label: "1920 × 1080 — landscape", w: 1920, h: 1080 },
  { label: "1080 × 1920 — vertical / Shorts", w: 1080, h: 1920 },
  { label: "3840 × 2160 — 4K", w: 3840, h: 2160 },
  { label: "1280 × 720 — 720p", w: 1280, h: 720 },
  { label: "1080 × 1080 — square", w: 1080, h: 1080 },
];
const RATES = ["30", "29.97", "25", "24", "23.976", "50", "60"];

function showNew() {
  view = "new";
  const p = page();
  const form = el("div", "form");
  const head = el("div", "head");
  head.appendChild(el("h1", "", "new project"));
  form.append(head, el("p", "lede", "kuma creates a folder with the project file inside it."));
  const errorBox = el("div", "error");
  errorBox.style.display = "none";
  form.appendChild(errorBox);

  const nameInput = el("input");
  nameInput.placeholder = "My video";
  nameInput.value = "";
  form.appendChild(field("Name", nameInput));

  const parentInput = el("input");
  parentInput.value = info.defaultParent;
  parentInput.spellcheck = false;
  const browse = button("browse…", async () => {
    const picked = await launcher.pickParent();
    if (picked) {
      parentInput.value = picked;
      void refreshHint();
    }
  });
  form.appendChild(field("Location", row(parentInput, browse)));

  const sizeSel = el("select");
  for (const s of SIZES) sizeSel.appendChild(new Option(s.label, `${s.w}x${s.h}`));
  sizeSel.appendChild(new Option("Custom…", "custom"));
  const customW = el("input");
  const customH = el("input");
  customW.type = customH.type = "number";
  customW.value = "1920";
  customH.value = "1080";
  const customRow = row(customW, el("span", "muted", "×"), customH);
  customRow.style.display = "none";
  sizeSel.onchange = () => {
    customRow.style.display = sizeSel.value === "custom" ? "flex" : "none";
  };

  const fpsSel = el("select");
  for (const r of RATES) fpsSel.appendChild(new Option(`${r} fps`, r));
  fpsSel.appendChild(new Option("Custom…", "custom"));
  const customFps = el("input");
  customFps.placeholder = "30000/1001";
  customFps.style.display = "none";
  fpsSel.onchange = () => {
    customFps.style.display = fpsSel.value === "custom" ? "block" : "none";
  };

  const grid = el("div", "grid2");
  const sizeField = field("Resolution", sizeSel);
  sizeField.appendChild(customRow);
  const fpsField = field("Frame rate", fpsSel);
  fpsField.appendChild(customFps);
  grid.append(sizeField, fpsField);
  form.appendChild(grid);

  const hint = el("div", "hint");
  form.appendChild(hint);

  const busy = el("div", "busy");
  busy.style.display = "none";
  const createBtn = button("create project", () => void submit(), "primary");
  const rowActions = el("div", "actions");
  const cancel = button("cancel", () => showRecents(), "ghost");
  cancel.appendChild(el("kbd", "", "esc"));
  rowActions.append(createBtn, cancel, busy);
  form.appendChild(rowActions);
  p.appendChild(form);

  const spec = (): NewProject => {
    const [w, h] = sizeSel.value === "custom" ? [Number(customW.value), Number(customH.value)] : sizeSel.value.split("x").map(Number);
    return {
      parentDir: parentInput.value.trim(),
      name: nameInput.value.trim(),
      width: w,
      height: h,
      fps: fpsSel.value === "custom" ? customFps.value.trim() : fpsSel.value,
    };
  };

  async function refreshHint() {
    const s = spec();
    if (!s.name || !s.parentDir) {
      hint.textContent = "";
      createBtn.disabled = true;
      return;
    }
    createBtn.disabled = false;
    const to = await launcher.previewPath(s.parentDir, s.name);
    hint.replaceChildren("will create ", el("span", "to", shortPath(to, 200)));
    hint.title = to;
  }

  async function submit() {
    const s = spec();
    if (!s.name || !s.parentDir) return;
    if (!(s.width > 0 && s.height > 0)) return fail("Width and height must be positive.");
    errorBox.style.display = "none";
    createBtn.disabled = true;
    busy.style.display = "block";
    busy.textContent = "creating the project and starting its daemon…";
    try {
      await launcher.create(s);
    } catch (e) {
      fail(message(e));
      createBtn.disabled = false;
      busy.style.display = "none";
    }
  }

  function fail(text: string) {
    errorBox.textContent = text;
    errorBox.style.display = "block";
    main.scrollTop = 0;
  }

  nameInput.oninput = () => void refreshHint();
  parentInput.oninput = () => void refreshHint();
  nameInput.onkeydown = (e) => {
    if (e.key === "Enter") void submit();
  };
  void refreshHint();
  nameInput.focus();
}

function field(label: string, control: HTMLElement): HTMLDivElement {
  const f = el("div", "field");
  f.append(el("label", "label", label), control);
  return f;
}

function row(...items: HTMLElement[]): HTMLDivElement {
  const r = el("div", "row");
  r.append(...items);
  return r;
}

// ---- opening ----

/** The main process shows its own dialog on failure; here we only need to stay usable. */
async function open(file: string) {
  try {
    await launcher.open(file);
  } catch {
    recents = await launcher.recents();
    if (view === "recents") showRecents();
  }
}

async function openDialog() {
  const picked = await launcher.pickProject();
  if (picked) await open(picked);
}

function message(e: unknown): string {
  const m = e instanceof Error ? e.message : String(e);
  // Electron prefixes IPC rejections with the channel; the user only wants the reason.
  return m.replace(/^Error invoking remote method '[^']+': ?/, "").replace(/^Error: /, "");
}

// ---- drag and drop ----

let dragDepth = 0;
window.addEventListener("dragenter", (e) => {
  e.preventDefault();
  if (++dragDepth === 1) dropHint.classList.add("on");
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("dragleave", (e) => {
  e.preventDefault();
  if (--dragDepth <= 0) {
    dragDepth = 0;
    dropHint.classList.remove("on");
  }
});
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  dropHint.classList.remove("on");
  const file = e.dataTransfer?.files?.[0];
  const p = file ? launcher.pathForFile(file) : "";
  if (p) void open(p);
});

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && view === "new") showRecents();
  // `/` finds, as it does in a pager or an editor, unless a field is already taking keys.
  if (e.key === "/" && view === "recents" && !(e.target instanceof HTMLInputElement)) {
    const search = document.querySelector<HTMLInputElement>(".filter input");
    if (search) {
      e.preventDefault();
      search.focus();
    }
  }
});

// ---- start ----

async function start() {
  [info, recents] = await Promise.all([launcher.info(), launcher.recents()]);
  renderChrome();
  showRecents();
  launcher.onCommand((name) => {
    if (name === "new-project" && info.kuma) showNew();
  });
  launcher.onRecentsChanged(async () => {
    recents = await launcher.recents();
    if (view === "recents") showRecents();
  });
}

void start();
