// Preload for an editor window. The page is remote content (the daemon serves it), so
// this bridge is deliberately tiny: which project the window shows, menu commands, a
// native file picker and revealing a folder. Nothing here can reach another project —
// the main process resolves every path against the window's own project root.

const { contextBridge, ipcRenderer, webUtils } = require("electron");

/** Values the main process passed in `additionalArguments`. */
const arg = (name) => {
  const prefix = `--kuma-${name}=`;
  const found = process.argv.find((a) => a.startsWith(prefix));
  return found ? found.slice(prefix.length) : "";
};

const projectFile = arg("project");
const sep = projectFile.includes("\\") && !projectFile.includes("/") ? "\\" : "/";

contextBridge.exposeInMainWorld("kumaApp", {
  version: arg("version"),
  projectFile,
  projectDir: projectFile.slice(0, projectFile.lastIndexOf(sep)),
  daemonUrl: arg("daemon"),
  /** False when the shell attached to a daemon somebody else started, and so will not stop it. */
  ownsDaemon: arg("owned") === "1",
  /** Native file picker; returns paths relative to the project root where possible. */
  pickMedia: () => ipcRenderer.invoke("media:pick"),
  /** Native file picker for .ttf/.otf; absolute paths. */
  pickFonts: () => ipcRenderer.invoke("fonts:pick"),
  /** Where a dropped file is on disk (`File.path` went in Electron 32). */
  pathForFile: (file) => webUtils.getPathForFile(file),
  /** Open a folder inside the project in the desktop file manager. */
  reveal: (relativePath) => ipcRenderer.invoke("shell:reveal", String(relativePath ?? ".")),
  showLauncher: () => ipcRenderer.invoke("app:show-launcher"),
  onCommand: (fn) => {
    const h = (_e, name) => fn(String(name));
    ipcRenderer.on("kuma:command", h);
    return () => ipcRenderer.off("kuma:command", h);
  },

  // The window draws its own title bar, so the page needs the menu names, a way to drop
  // the real (native) submenu under one, and the window buttons.
  menuLabels: () => ipcRenderer.invoke("menu:labels"),
  popupMenu: (label, x, y) => ipcRenderer.invoke("menu:popup", String(label), Number(x), Number(y)),
  onMenuChanged: (fn) => {
    const h = (_e, labels) => fn(labels);
    ipcRenderer.on("kuma:menu", h);
    return () => ipcRenderer.off("kuma:menu", h);
  },
  windowControl: (action) => ipcRenderer.invoke("window:control", String(action)),
  windowState: () => ipcRenderer.invoke("window:state"),
  onWindowState: (fn) => {
    const h = (_e, state) => fn(state);
    ipcRenderer.on("kuma:window-state", h);
    return () => ipcRenderer.off("kuma:window-state", h);
  },
});
