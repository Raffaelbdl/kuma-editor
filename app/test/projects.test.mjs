// Creating and vetting a project. The interesting cases are the ones a wrong `kuma`
// binary causes, so a stand-in script takes its place: no Rust build needed, and the
// failure it produces is the exact one a stale binary produced in practice.

import { strict as assert } from "node:assert";
import { after, describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const projects = require("../dist/projects.js");
const REPO = path.resolve(import.meta.dirname, "..", "..");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kuma-projects-test-"));
after(() => fs.rmSync(tmp, { recursive: true, force: true }));

/** A fake `kuma` whose `init` writes `body` at `--project`. */
function fakeKuma(name, body) {
  const bin = path.join(tmp, name);
  fs.writeFileSync(bin, `#!/bin/sh
out=""
while [ $# -gt 0 ]; do case "$1" in --project) out="$2"; shift 2;; *) shift;; esac; done
mkdir -p "$(dirname "$out")"
cat > "$out" <<'JSON'
${body}
JSON
exit 0
`);
  fs.chmodSync(bin, 0o755);
  return bin;
}

const GOOD = '{"version":1,"name":"x","master":"main","bin":{"main":{"name":"Main","source":{"tracks":[],"markers":[]}}}}';
const OLD_FORMAT = '{"version":1,"name":"x","assets":{},"tracks":[],"markers":[]}';

describe("checkOpenable", () => {
  const write = (name, text) => {
    const f = path.join(tmp, name);
    fs.writeFileSync(f, text);
    return f;
  };

  it("accepts a project", () => {
    projects.checkOpenable(write("ok.kuma.json", GOOD));
  });

  it("names the old format rather than blaming the file", () => {
    // The cause is a `kuma` older than the bin refactor, and saying so is the difference
    // between rebuilding and reading a file that is fine for whatever wrote it.
    assert.throws(() => projects.checkOpenable(write("old.kuma.json", OLD_FORMAT)), (e) => {
      assert.match(e.message, /old project format/);
      assert.match(e.message, /cargo build --release/);
      return true;
    });
  });

  it("lists what a non-project json does have", () => {
    assert.throws(() => projects.checkOpenable(write("mcp.json", '{"mcpServers":{}}')), /no `master` and `bin`[\s\S]*mcpServers/);
  });

  it("rejects invalid json and a missing file", () => {
    assert.throws(() => projects.checkOpenable(write("bad.json", "{oops")), /not valid JSON/);
    assert.throws(() => projects.checkOpenable(path.join(tmp, "nope.json")), /does not exist/);
  });
});

describe("create", () => {
  it("slugs the name into a folder and file", async () => {
    const parent = path.join(tmp, "good");
    const file = await projects.create(fakeKuma("kuma-good", GOOD), REPO, { parentDir: parent, name: "My First Video", width: 1920, height: 1080, fps: "30" });
    assert.equal(file, path.join(parent, "my-first-video", "my-first-video.kuma.json"));
    assert.ok(fs.existsSync(file));
  });

  it("rolls back what a stale binary wrote, so the name is free again", async () => {
    const parent = path.join(tmp, "stale");
    const bin = fakeKuma("kuma-stale", OLD_FORMAT);
    const spec = { parentDir: parent, name: "testangular", width: 1920, height: 1080, fps: "30" };
    await assert.rejects(projects.create(bin, REPO, spec), /cannot open[\s\S]*old project format/);
    // A folder left behind blocks the retry while being useless — the reported bug.
    assert.equal(fs.existsSync(path.join(parent, "testangular")), false);
  });

  it("refuses a non-empty folder without touching it", async () => {
    const parent = path.join(tmp, "occupied");
    const mine = path.join(parent, "keepme");
    fs.mkdirSync(mine, { recursive: true });
    fs.writeFileSync(path.join(mine, "notes.txt"), "mine");
    await assert.rejects(
      projects.create(fakeKuma("kuma-ok2", GOOD), REPO, { parentDir: parent, name: "keepme", width: 1920, height: 1080, fps: "30" }),
      /already exists and is not empty/,
    );
    assert.equal(fs.readFileSync(path.join(mine, "notes.txt"), "utf8"), "mine");
  });
});

describe("slug", () => {
  it("makes an id the daemon will accept", () => {
    assert.equal(projects.slug("My First Video"), "my-first-video");
    assert.equal(projects.slug("  Ünïcode! **stuff** "), "n-code-stuff");
    assert.equal(projects.slug("///"), "");
  });
});
