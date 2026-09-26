// Daemon lifecycle. The shell owns every daemon it starts: port, health, restart, and
// the kill when the window closes or the app quits.
//
// The one it does **not** own is a daemon already serving the same project file. Two
// daemons writing one `.kuma.json` would each believe they own it — the invariant the
// design rests on — and that one is usually a `kuma mcp`. So the shell attaches instead,
// and leaves it running.

import { ChildProcess, spawn } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";


/** Where a hand-run `kuma serve` and `kuma mcp` look by default. */
const DEFAULT_PORT = 7455;
const START_TIMEOUT_MS = 30_000;
const KILL_GRACE_MS = 4_000;

export interface Daemon {
  /** Canonical project file this daemon serves. */
  projectFile: string;
  port: number;
  url: string;
  /** True when this shell spawned it, and therefore kills it. */
  owned: boolean;
  /** Set when `owned` and the process is alive. */
  child: ChildProcess | null;
  /** Tail of the daemon's stderr, kept so a crash can be reported with its reason. */
  log: string[];
}

export interface StartOptions {
  bin: string;
  repo: string;
  projectFile: string;
  /** Called if an owned daemon exits on its own (crash, or killed from outside). */
  onExit: (d: Daemon, code: number | null, signal: string | null) => void;
}

/** `/api/health` from a daemon on `port`, or null if nothing healthy answers. */
async function health(port: number, timeoutMs = 1500): Promise<Record<string, unknown> | null> {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/api/health`, { signal: AbortSignal.timeout(timeoutMs) });
    if (!r.ok) return null;
    return (await r.json()) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function realpath(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}

/** True when the daemon on `port` serves exactly `projectFile`. */
async function serves(port: number, projectFile: string): Promise<boolean> {
  const h = await health(port);
  const theirs = typeof h?.project === "string" ? h.project : null;
  return theirs !== null && realpath(theirs) === realpath(projectFile);
}

async function portFree(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const srv = net.createServer();
    srv.once("error", () => resolve(false));
    srv.once("listening", () => srv.close(() => resolve(true)));
    srv.listen(port, "127.0.0.1");
  });
}

/** An ephemeral port the OS says is free. Racy in principle; the daemon's bind is the
 *  real arbiter, and a lost race surfaces as a start failure rather than a wrong port. */
async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      srv.close(() => (port ? resolve(port) : reject(new Error("could not get a free port"))));
    });
  });
}

/**
 * The port to serve `projectFile` on. 7455 is preferred because that is where
 * `kuma mcp` — and so the `.mcp.json` that `kuma init` writes — looks for a daemon:
 * a project opened there is a project Claude Code can join without configuration.
 */
async function choosePort(projectFile: string): Promise<{ port: number; attach: boolean }> {
  if (await serves(DEFAULT_PORT, projectFile)) return { port: DEFAULT_PORT, attach: true };
  if (await portFree(DEFAULT_PORT)) return { port: DEFAULT_PORT, attach: false };
  return { port: await freePort(), attach: false };
}

export async function start(opts: StartOptions): Promise<Daemon> {
  const projectFile = path.resolve(opts.projectFile);
  const { port, attach } = await choosePort(projectFile);
  const url = `http://127.0.0.1:${port}`;

  if (attach) {
    return { projectFile, port, url, owned: false, child: null, log: [] };
  }

  // Which library it reads is the daemon's rule (`library_dirs`); the launcher only reports it.
  const child = spawn(opts.bin, ["serve", "--project", projectFile, "--bind", `127.0.0.1:${port}`], {
    cwd: path.dirname(projectFile),
    env: { ...process.env, KUMA_REPO: opts.repo },
    stdio: ["ignore", "pipe", "pipe"],
  });

  const daemon: Daemon = { projectFile, port, url, owned: true, child, log: [] };
  const record = (chunk: Buffer) => {
    for (const line of chunk.toString().split("\n")) {
      if (!line.trim()) continue;
      daemon.log.push(line);
      if (daemon.log.length > 200) daemon.log.shift();
    }
  };
  child.stdout?.on("data", record);
  child.stderr?.on("data", record);

  let settled = false;
  child.on("exit", (code, signal) => {
    daemon.child = null;
    if (settled) opts.onExit(daemon, code, signal);
  });
  child.on("error", (e) => {
    daemon.log.push(`spawn failed: ${e.message}`);
    daemon.child = null;
  });

  // The daemon loads and validates the project before it binds, so a bad project file
  // shows up as an exit with a reason on stderr rather than as a timeout.
  const deadline = Date.now() + START_TIMEOUT_MS;
  for (;;) {
    if (await serves(port, projectFile)) {
      settled = true;
      return daemon;
    }
    if (daemon.child === null) {
      throw new Error(`The kuma daemon exited while starting.\n\n${daemon.log.join("\n") || "(no output)"}`);
    }
    if (Date.now() > deadline) {
      stop(daemon);
      throw new Error(`The kuma daemon did not answer on ${url} within ${START_TIMEOUT_MS / 1000}s.\n\n${daemon.log.join("\n")}`);
    }
    await new Promise((r) => setTimeout(r, 150));
  }
}

/** Stop a daemon this shell started. Attached daemons are left alone. */
export function stop(d: Daemon): void {
  const child = d.child;
  if (!d.owned || !child || child.exitCode !== null) return;
  d.child = null;
  child.removeAllListeners("exit");
  child.kill("SIGTERM");
  // The daemon's own shutdown is a ctrl-c handler; give it a moment before insisting.
  const timer = setTimeout(() => {
    if (child.exitCode === null) child.kill("SIGKILL");
  }, KILL_GRACE_MS);
  child.once("exit", () => clearTimeout(timer));
}

/** The reason an owned daemon is gone, for the dialog that offers to restart it. */
export function exitReason(d: Daemon, code: number | null, signal: string | null): string {
  const how = signal ? `killed by ${signal}` : `exited with code ${code ?? "?"}`;
  const tail = d.log.slice(-12).join("\n");
  return tail ? `${how}\n\n${tail}` : how;
}
