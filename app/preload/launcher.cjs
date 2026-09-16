// Preload for the launcher page. Hand-written CommonJS: it is the security boundary, so
// it stays small enough to read in one go and exposes named calls, never `ipcRenderer`.

const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("launcher", {
  info: () => ipcRenderer.invoke("app:info"),
  recents: () => ipcRenderer.invoke("recents:list"),
  forget: (file) => ipcRenderer.invoke("recents:remove", String(file)),
  clearRecents: () => ipcRenderer.invoke("recents:clear"),
  pickProject: () => ipcRenderer.invoke("project:pick"),
  pickParent: () => ipcRenderer.invoke("project:pick-parent"),
  previewPath: (parentDir, name) => ipcRenderer.invoke("project:preview-path", String(parentDir), String(name)),
  create: (spec) => ipcRenderer.invoke("project:create", spec),
  open: (file) => ipcRenderer.invoke("project:open", String(file)),
  /** Menu-driven actions that the launcher page has to react to ("New Project…"). */
  onCommand: (fn) => {
    const h = (_e, name) => fn(String(name));
    ipcRenderer.on("kuma:command", h);
    return () => ipcRenderer.off("kuma:command", h);
  },
  onRecentsChanged: (fn) => {
    const h = () => fn();
    ipcRenderer.on("recents:changed", h);
    return () => ipcRenderer.off("recents:changed", h);
  },
});
