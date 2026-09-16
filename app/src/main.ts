// The kuma desktop shell.
//
// Two kinds of window. The **launcher** is a local page: it lists recent projects and
// creates or opens one. An **editor** window is the existing web UI, loaded from the
// daemon serving that project — the same UI a browser gets, so nothing here forks it.
//
// One project, one daemon, one window. That is the shape the rest of kuma already
// assumes (`kuma mcp` finds a daemon by the project it serves), so opening a second
// project starts a second daemon rather than teaching the first to switch.

import { app, BrowserWindow, dialog, ipcMain, Menu, MenuItemConstructorOptions, net, protocol, shell } from "electron";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

import * as daemon from "./daemon";
import type { Daemon } from "./daemon";
import * as projects from "./projects";
import { Recents, nameOf } from "./recents";
import { Kuma, repoWarnings, resolveKuma, uiBuildProblem } from "./kuma";

const APP_DIR = path.resolve(__dirname, "..");
const LAUNCHER_ORIGIN = "kuma-app://launcher";

let kuma: Kuma | null = null;
let kumaError: string | null = null;
let recents: Recents;
let launcher: BrowserWindow | null = null;
/** Every open editor window, and the daemon behind it. */
const editors = new Map<BrowserWindow, Daemon>();
/** Projects with a start in flight, so a double click cannot open two daemons for one. */
const starting = new Set<string>();
let quitting = false;

// ---------------------------------------------------------------- launcher assets

// A real scheme rather than `file://`: ES modules, a meaningful origin for the CSP, and
// no directory traversal from a relative URL.
protocol.registerSchemesAsPrivileged([{ scheme: "kuma-app", privileges: { standard: true, secure: true, supportFetchAPI: true } }]);

function serveLauncher() {
  const root = path.join(APP_DIR, "dist", "launcher");
  protocol.handle("kuma-app", async (req) => {
    const url = new URL(req.url);
    if (url.host !== "launcher") return new Response("not found", { status: 404 });
    const rel = decodeURIComponent(url.pathname).replace(/^\/+/, "") || "index.html";
    const file = path.join(root, rel);
    // `path.join` normalizes `..` away, so re-check that we stayed inside the root.
    if (!file.startsWith(root + path.sep)) return new Response("forbidden", { status: 403 });
    if (!fs.existsSync(file)) return new Response("not found", { status: 404 });
    const res = await net.fetch(pathToFileURL(file).toString());
    // These files are read off disk next to the app; a cached copy only ever hides a
    // rebuild from the window that is already open.
    const headers = new Headers(res.headers);
    headers.set("cache-control", "no-store");
    return new Response(res.body, { status: res.status, headers });
  });
}

// ---------------------------------------------------------------- windows

function showLauncher() {
  if (launcher && !launcher.isDestroyed()) {
    launcher.show();
    launcher.focus();
    launcher.webContents.send("recents:changed");
    return;
  }
  launcher = new BrowserWindow({
    width: 940,
    height: 660,
    minWidth: 720,
    minHeight: 520,
    title: "kuma",
    backgroundColor: "#121317",
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(APP_DIR, "preload", "launcher.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  launcher.once("ready-to-show", () => launcher?.show());
  launcher.on("closed", () => {
    launcher = null;
    // Closing the launcher with nothing else open means "I am done".
    if (editors.size === 0 && !quitting) app.quit();
  });
  launcher.loadURL(`${LAUNCHER_ORIGIN}/index.html`);
}

function hideLauncher() {
  if (launcher && !launcher.isDestroyed()) launcher.hide();
}

function editorFor(d: Daemon): BrowserWindow {
  const win = new BrowserWindow({
    width: 1500,
    height: 940,
    minWidth: 1000,
    minHeight: 640,
    title: `${nameOf(d.projectFile)} — kuma`,
    backgroundColor: "#121317",
    show: false,
    webPreferences: {
      preload: path.join(APP_DIR, "preload", "editor.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      // Read by the preload, which has no other way to learn which project this is:
      // the page itself is remote content served by the daemon.
      additionalArguments: [
        `--kuma-project=${d.projectFile}`,
        `--kuma-daemon=${d.url}`,
        `--kuma-owned=${d.owned ? "1" : "0"}`,
        `--kuma-version=${app.getVersion()}`,
      ],
    },
  });
  // The UI's <title> is just "kuma"; keep the project name in the title bar.
  win.on("page-title-updated", (e) => e.preventDefault());
  win.once("ready-to-show", () => win.show());
  // External links belong in the user's browser, never in a new chromeless window.
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:$/.test(new URL(url).protocol)) shell.openExternal(url);
    return { action: "deny" };
  });
  // This window carries a preload with native calls in it. They are meant for the page
  // the daemon serves and nothing else, so navigating away is refused rather than
  // handing that bridge to another origin.
  win.webContents.on("will-navigate", (e, url) => {
    if (new URL(url).origin === d.url) return;
    e.preventDefault();
    if (/^https?:$/.test(new URL(url).protocol)) shell.openExternal(url);
  });
  win.on("closed", () => {
    editors.delete(win);
    daemon.stop(d);
    rebuildMenu();
    if (editors.size === 0 && !quitting) showLauncher();
  });
  win.loadURL(d.url);
  return win;
}

// ---------------------------------------------------------------- opening

/** Focus the window already showing `file`, if there is one. */
function existingWindow(file: string): BrowserWindow | null {
  const want = path.resolve(file);
  for (const [win, d] of editors) if (path.resolve(d.projectFile) === want) return win;
  return null;
}

async function openProject(file: string, from?: BrowserWindow): Promise<void> {
  const target = path.resolve(file);
  const open = existingWindow(target);
  if (open) {
    open.show();
    open.focus();
    hideLauncher();
    return;
  }
  if (starting.has(target)) return;
  if (!kuma) throw new Error(kumaError ?? "kuma is not available");

  projects.checkOpenable(target);
  // A stale or missing `ui/dist` is not a warning to read later: the editor window loads
  // that bundle, and an old one fails on the shape of a newer project with an error that
  // reads like a broken document. Say so before opening the window, not after.
  const uiProblem = uiBuildProblem(kuma.repo);
  if (uiProblem && !(await confirmStaleUi(uiProblem, from))) return;
  starting.add(target);
  try {
    const d = await daemon.start({
      bin: kuma.bin,
      repo: kuma.repo,
      projectFile: target,
      onExit: (dead, code, signal) => reportDaemonExit(dead, code, signal),
    });
    const win = editorFor(d);
    editors.set(win, d);
    recents.touch(target);
    hideLauncher();
    rebuildMenu();
  } catch (e) {
    // A project that cannot be opened stays in the list: the fix is usually elsewhere
    // (a missing binary, a busy port), not "forget this project".
    const opts: Electron.MessageBoxOptions = {
      type: "error",
      title: "Cannot open project",
      message: `Could not open ${path.basename(target)}`,
      detail: (e as Error).message,
      buttons: ["OK"],
    };
    const parent = from && !from.isDestroyed() ? from : (launcher ?? undefined);
    await (parent ? dialog.showMessageBox(parent, opts) : dialog.showMessageBox(opts));
    throw e;
  } finally {
    starting.delete(target);
  }
}

/** Offer to open anyway: the check is an mtime heuristic, so it must never be a hard no. */
async function confirmStaleUi(problem: string, from?: BrowserWindow): Promise<boolean> {
  const opts: Electron.MessageBoxOptions = {
    type: "warning",
    title: "The editor build is out of date",
    message: "The editor would not load correctly.",
    detail: `${problem}\n\nThe daemon serves ui/dist, and it is not checked into git, so pulling a branch leaves it behind.`,
    buttons: ["Cancel", "Open anyway"],
    defaultId: 0,
    cancelId: 0,
  };
  const parent = from && !from.isDestroyed() ? from : (launcher ?? undefined);
  const { response } = await (parent ? dialog.showMessageBox(parent, opts) : dialog.showMessageBox(opts));
  return response === 1;
}

/** An owned daemon died on its own. Offer to bring it back rather than leaving a blank window. */
async function reportDaemonExit(dead: Daemon, code: number | null, signal: string | null) {
  if (quitting) return;
  const entry = [...editors].find(([, d]) => d === dead);
  if (!entry) return;
  const [win] = entry;
  const { response } = await dialog.showMessageBox(win, {
    type: "error",
    title: "The kuma daemon stopped",
    message: `The daemon serving ${nameOf(dead.projectFile)} stopped.`,
    detail: daemon.exitReason(dead, code, signal),
    buttons: ["Restart", "Close Window"],
    defaultId: 0,
    cancelId: 1,
  });
  editors.delete(win);
  if (response === 0) {
    win.destroy();
    await openProject(dead.projectFile).catch(() => showLauncher());
  } else {
    win.destroy();
    if (editors.size === 0) showLauncher();
  }
}

// ---------------------------------------------------------------- menu

function sendCommand(command: string) {
  const win = BrowserWindow.getFocusedWindow();
  if (win && editors.has(win)) win.webContents.send("kuma:command", command);
}

function projectDirOfFocused(): string | null {
  const win = BrowserWindow.getFocusedWindow();
  const d = win ? editors.get(win) : undefined;
  return d ? path.dirname(d.projectFile) : null;
}

function rebuildMenu() {
  const recentItems: MenuItemConstructorOptions[] = recents
    .list()
    .slice(0, 12)
    .map((r) => ({
      label: r.missing ? `${r.name} (missing)` : r.name,
      enabled: !r.missing,
      toolTip: r.path,
      click: () => void openProject(r.path).catch(() => {}),
    }));
  if (recentItems.length) recentItems.push({ type: "separator" }, { label: "Clear Menu", click: () => { recents.clear(); rebuildMenu(); launcher?.webContents.send("recents:changed"); } });
  else recentItems.push({ label: "No recent projects", enabled: false });

  const template: MenuItemConstructorOptions[] = [
    {
      label: "File",
      submenu: [
        { label: "New Project…", accelerator: "CmdOrCtrl+N", click: () => void newProjectFlow() },
        { label: "Open Project…", accelerator: "CmdOrCtrl+O", click: () => void openProjectFlow() },
        { label: "Open Recent", submenu: recentItems },
        { type: "separator" },
        { label: "Project Launcher", accelerator: "CmdOrCtrl+Shift+L", click: () => showLauncher() },
        { type: "separator" },
        {
          label: "Reveal Project Folder",
          click: () => {
            const dir = projectDirOfFocused();
            if (dir) shell.openPath(dir);
          },
        },
        { type: "separator" },
        { role: "close" },
        { role: "quit" },
      ],
    },
    // Native roles: these are text editing, and must keep working inside the inspector's
    // fields. Project undo is a different thing and lives in the Project menu.
    { label: "Edit", submenu: [{ role: "undo" }, { role: "redo" }, { type: "separator" }, { role: "cut" }, { role: "copy" }, { role: "paste" }, { role: "selectAll" }] },
    {
      label: "Project",
      submenu: [
        // No accelerators: the editor already binds Ctrl+Z, Space and S in the page, where
        // it can tell whether a text field has focus. A menu accelerator cannot, and would
        // fire while typing.
        { label: "Undo Edit", click: () => sendCommand("undo") },
        { label: "Redo Edit", click: () => sendCommand("redo") },
        { type: "separator" },
        { label: "Add Media…", click: () => sendCommand("add-media") },
        { label: "New Sequence…", click: () => sendCommand("new-sequence") },
        { type: "separator" },
        { label: "Reload Preview", click: () => sendCommand("reload-preview") },
        { label: "Render", accelerator: "CmdOrCtrl+Shift+E", click: () => sendCommand("render") },
      ],
    },
    {
      label: "View",
      submenu: [
        { role: "reload" },
        { role: "forceReload" },
        { role: "toggleDevTools" },
        { type: "separator" },
        { role: "resetZoom" },
        { role: "zoomIn" },
        { role: "zoomOut" },
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    { label: "Window", submenu: [{ role: "minimize" }, { role: "zoom" }, { type: "separator" }, { role: "front" }] },
    {
      label: "Help",
      submenu: [
        { label: "kuma on GitHub", click: () => shell.openExternal("https://github.com/raffaelbdl/kuma-editor") },
        {
          label: "About kuma",
          click: () =>
            dialog.showMessageBox({
              type: "info",
              title: "kuma",
              message: `kuma ${app.getVersion()}`,
              detail: kuma ? `${kuma.version}\n\nbinary: ${kuma.bin}\nrepo: ${kuma.repo}` : (kumaError ?? "kuma binary not found"),
              buttons: ["OK"],
            }),
        },
      ],
    },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

// ---------------------------------------------------------------- flows

async function openProjectFlow(parent?: BrowserWindow) {
  const win = parent ?? BrowserWindow.getFocusedWindow() ?? undefined;
  const r = await (win ? dialog.showOpenDialog(win, openOpts()) : dialog.showOpenDialog(openOpts()));
  if (r.canceled || !r.filePaths[0]) return;
  await openProject(r.filePaths[0], win).catch(() => {});
}

function openOpts(): Electron.OpenDialogOptions {
  return { title: "Open kuma project", properties: ["openFile"], filters: projects.PROJECT_FILTERS };
}

/** "New project" from the menu: pick a folder, then let the launcher collect the details. */
async function newProjectFlow() {
  showLauncher();
  launcher?.webContents.send("kuma:command", "new-project");
}

// ---------------------------------------------------------------- ipc

function wireIpc() {
  ipcMain.handle("app:info", () => ({
    version: app.getVersion(),
    kuma: kuma ? { bin: kuma.bin, repo: kuma.repo, version: kuma.version } : null,
    error: kumaError,
    warnings: kuma ? repoWarnings(kuma.repo, kuma.version) : [],
    defaultParent: app.getPath("home"),
  }));

  ipcMain.handle("recents:list", () => recents.list());
  ipcMain.handle("recents:remove", (_e, file: string) => {
    recents.remove(file);
    rebuildMenu();
    return recents.list();
  });
  ipcMain.handle("recents:clear", () => {
    recents.clear();
    rebuildMenu();
    return recents.list();
  });

  ipcMain.handle("project:pick", async (e) => {
    const win = BrowserWindow.fromWebContents(e.sender);
    const r = await (win ? dialog.showOpenDialog(win, openOpts()) : dialog.showOpenDialog(openOpts()));
    return r.canceled ? null : (r.filePaths[0] ?? null);
  });

  ipcMain.handle("project:pick-parent", async (e) => {
    const win = BrowserWindow.fromWebContents(e.sender);
    const opts: Electron.OpenDialogOptions = { title: "Where should the project folder go?", properties: ["openDirectory", "createDirectory"] };
    const r = await (win ? dialog.showOpenDialog(win, opts) : dialog.showOpenDialog(opts));
    return r.canceled ? null : (r.filePaths[0] ?? null);
  });

  ipcMain.handle("project:preview-path", (_e, parentDir: string, name: string) => projects.projectFileFor(parentDir, name));

  ipcMain.handle("project:create", async (_e, spec: projects.NewProject) => {
    if (!kuma) throw new Error(kumaError ?? "kuma is not available");
    const file = await projects.create(kuma.bin, kuma.repo, spec);
    await openProject(file);
    return file;
  });

  ipcMain.handle("project:open", async (_e, file: string) => {
    await openProject(file);
    return true;
  });

  ipcMain.handle("app:show-launcher", () => showLauncher());

  // ---- editor-window calls ----

  /** The daemon behind the window that called, so a renderer can only act on its own project. */
  const daemonOf = (e: Electron.IpcMainInvokeEvent): Daemon => {
    const win = BrowserWindow.fromWebContents(e.sender);
    const d = win ? editors.get(win) : undefined;
    if (!d) throw new Error("not an editor window");
    return d;
  };

  ipcMain.handle("media:pick", async (e) => {
    const d = daemonOf(e);
    const win = BrowserWindow.fromWebContents(e.sender)!;
    const r = await dialog.showOpenDialog(win, {
      title: "Add media",
      defaultPath: path.dirname(d.projectFile),
      properties: ["openFile", "multiSelections"],
      filters: [
        { name: "Media", extensions: ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "aac", "flac", "ogg", "m4a", "png", "jpg", "jpeg", "webp", "gif", "tif", "tiff"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (r.canceled) return [];
    // Relative to the project root when the file is inside it, so the project stays
    // portable; absolute otherwise, which `resolve_path` accepts unchanged.
    const root = path.dirname(d.projectFile);
    return r.filePaths.map((p) => {
      const rel = path.relative(root, p);
      return rel && !rel.startsWith("..") && !path.isAbsolute(rel) ? rel : p;
    });
  });

  ipcMain.handle("shell:reveal", (e, rel: string) => {
    const d = daemonOf(e);
    const root = path.dirname(d.projectFile);
    const target = path.resolve(root, rel || ".");
    // Never open a path outside the project from a page the daemon serves.
    if (target !== root && !target.startsWith(root + path.sep)) throw new Error("outside the project");
    return fs.existsSync(target) ? shell.openPath(fs.statSync(target).isDirectory() ? target : path.dirname(target)) : shell.openPath(root);
  });
}

// ---------------------------------------------------------------- lifecycle

// One instance owns the daemons; a second would start duplicates for the same projects.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", (_e, argv) => {
    const file = argv.find((a) => a.endsWith(".json") && fs.existsSync(a));
    if (file) void openProject(file).catch(() => {});
    else showLauncher();
  });

  app.whenReady().then(() => {
    try {
      kuma = resolveKuma(APP_DIR);
    } catch (e) {
      kumaError = (e as Error).message;
    }
    recents = new Recents(app.getPath("userData"));
    serveLauncher();
    wireIpc();
    rebuildMenu();

    // `kuma-app project.kuma.json` opens straight into the editor.
    const arg = process.argv.slice(1).find((a) => a.endsWith(".json") && fs.existsSync(a));
    if (arg && kuma) void openProject(arg).catch(() => showLauncher());
    else showLauncher();

    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0) showLauncher();
    });
  });

  app.on("open-file", (e, file) => {
    e.preventDefault();
    void openProject(file).catch(() => {});
  });

  // Every daemon this shell started goes with it — that is the whole point of the shell
  // owning them. `will-quit` rather than `window-all-closed`, so a closed last window
  // that reopens the launcher does not take the app down with it.
  app.on("before-quit", () => {
    quitting = true;
  });
  app.on("will-quit", () => {
    for (const d of editors.values()) daemon.stop(d);
    editors.clear();
  });
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin" && quitting) app.quit();
  });
}
