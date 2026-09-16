// Preload for an editor window. The page is remote content (the daemon serves it), so
// this bridge is deliberately tiny: which project the window shows, menu commands, a
// native file picker and revealing a folder. Nothing here can reach another project —
// the main process resolves every path against the window's own project root.

const { contextBridge, ipcRenderer } = require("electron");

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
  /** Open a folder inside the project in the desktop file manager. */
  reveal: (relativePath) => ipcRenderer.invoke("shell:reveal", String(relativePath ?? ".")),
  showLauncher: () => ipcRenderer.invoke("app:show-launcher"),
  onCommand: (fn) => {
    const h = (_e, name) => fn(String(name));
    ipcRenderer.on("kuma:command", h);
    return () => ipcRenderer.off("kuma:command", h);
  },
});
