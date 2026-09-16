// What the shell decides before it starts anything: which binary, which library, and
// whether either is too old to trust. All of it is pure filesystem reasoning, so it runs
// without Electron, a daemon or a display.

import { strict as assert } from "node:assert";
import { after, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const kuma = require("../dist/kuma.js");
const REPO = path.resolve(import.meta.dirname, "..", "..");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-app-test-"));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

/** Run `fn` with these env vars set, restoring whatever was there. */
function withEnv(vars, fn) {
  const saved = {};
  for (const [k, v] of Object.entries(vars)) {
    saved[k] = process.env[k];
    if (v === undefined) delete process.env[k];
    else process.env[k] = v;
  }
  try {
    return fn();
  } finally {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  }
}

const dir = (...parts) => {
  const p = path.join(tmp, ...parts);
  fs.mkdirSync(p, { recursive: true });
  return p;
};

describe("library fallback", () => {
  it("points at the checkout when nothing is installed", () => {
    const xdg = dir("empty", ".local", "share");
    const got = withEnv({ XDG_DATA_HOME: xdg, KUMA_LIBRARY: undefined }, () => kuma.libraryFallback(REPO));
    assert.equal(got, path.join(REPO, "library"));
  });

  it("stays out of the way when a library is installed", () => {
    const xdg = dir("installed", ".local", "share");
    dir("installed", ".local", "share", "kuma", "overlays", "mine");
    // Otherwise the checkout's copy of a template would shadow one they had edited.
    assert.equal(withEnv({ XDG_DATA_HOME: xdg, KUMA_LIBRARY: undefined }, () => kuma.libraryFallback(REPO)), null);
  });

  it("never overrides an explicit KUMA_LIBRARY", () => {
    const xdg = dir("empty2", ".local", "share");
    assert.equal(withEnv({ XDG_DATA_HOME: xdg, KUMA_LIBRARY: "/somewhere/else" }, () => kuma.libraryFallback(REPO)), null);
  });

  it("treats an empty overlays/ as nothing installed", () => {
    const xdg = dir("hollow", ".local", "share");
    dir("hollow", ".local", "share", "kuma", "overlays");
    assert.equal(withEnv({ XDG_DATA_HOME: xdg, KUMA_LIBRARY: undefined }, () => kuma.libraryFallback(REPO)), path.join(REPO, "library"));
  });

  it("invents nothing when the repo ships no library", () => {
    const xdg = dir("empty3", ".local", "share");
    assert.equal(withEnv({ XDG_DATA_HOME: xdg, KUMA_LIBRARY: undefined }, () => kuma.libraryFallback(dir("bare-repo"))), null);
  });
});

describe("stale build detection", () => {
  const warns = (repo, version) => kuma.repoWarnings(repo, version).join(" | ");

  it("reports a binary built from a commit HEAD has passed", () => {
    // A real ancestor of this checkout: the merge the desktop work started from.
    assert.match(warns(REPO, "kuma 0.1.0 (657ec66c03)"), /commits behind this checkout/);
  });

  it("says nothing about a commit this repo has never seen", () => {
    assert.doesNotMatch(warns(REPO, "kuma 0.1.0 (deadbeef1234)"), /behind this checkout/);
  });

  it("says nothing when the version carries no commit", () => {
    assert.doesNotMatch(warns(REPO, "kuma 0.1.0"), /behind this checkout/);
  });

  it("reports ui/dist older than ui/src, and nothing when it is newer", () => {
    const root = dir("ui-stale");
    const distFile = path.join(dir("ui-stale", "ui", "dist"), "index.js");
    const srcFile = path.join(dir("ui-stale", "ui", "src"), "main.ts");
    fs.writeFileSync(distFile, "//");
    fs.writeFileSync(srcFile, "//");
    const touch = (f, whenMs) => fs.utimesSync(f, whenMs / 1000, whenMs / 1000);
    const now = Date.now();

    touch(distFile, now - 600_000);
    touch(srcFile, now);
    assert.match(kuma.repoWarnings(root).join(" | "), /ui\/dist is older than ui\/src/);

    touch(distFile, now);
    touch(srcFile, now - 600_000);
    assert.doesNotMatch(kuma.repoWarnings(root).join(" | "), /ui\/dist is older/);
  });

  it("calls a missing ui/dist missing, not stale", () => {
    const root = dir("ui-absent");
    const w = kuma.repoWarnings(root).join(" | ");
    assert.match(w, /ui\/dist is missing/);
    assert.doesNotMatch(w, /older than/);
  });
});
