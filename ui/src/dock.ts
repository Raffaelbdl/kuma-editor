// The dock: power mode's layout, as a tree.
//
// A **split** lays its children out in a row or a column; a **group** is a stack of panels
// shown one at a time, as tabs; the **viewer** — the frame and its transport — is the one
// leaf that is not a panel and cannot be closed. Dragging a tab onto a group's middle adds
// it to that group; onto one of its edges, or the viewer's, or the dock's own, splits the
// space there. Pure data and pure operations: `panelhost.ts` draws it and remembers it.
//
// Sizes are pixels along the parent's axis. In the split that holds the viewer, the child
// with the viewer is the **fill** and takes what the others leave — that is how a side panel
// keeps its width when the window grows, as an editor's sidebar does. In a split without
// it, the children share the space in proportion to their sizes, so none is squeezed out.

export type Dir = "row" | "col";
export type Zone = "center" | "left" | "right" | "top" | "bottom";

export interface Split { id: string; kind: "split"; dir: Dir; children: Node[]; size?: number }
export interface Group { id: string; kind: "group"; tabs: string[]; active: string; size?: number }
export interface Viewer { id: "viewer"; kind: "viewer"; size?: number }
export type Node = Split | Group | Viewer;

/** Where a dragged panel lands: a node and the part of it, or an edge of the whole dock. */
export type Target = { node: string; zone: Zone } | { root: Exclude<Zone, "center"> };

/** The panels that live in each place, as their specs say (`PanelSpec.dock`). */
export type Homes = Record<"side" | "right" | "bottom", string[]>;

/** Power's layout as it was when the dock had fixed slots: the side panels as one group of
 *  tabs, the frame, the right's panels, and the bottom's across the bottom. */
export function defaultTree(homes: Homes): Node {
  const group = (id: string, tabs: string[], size: number): Group => ({ id, kind: "group", tabs, active: tabs[0] ?? "", size });
  return normalize({
    id: "root", kind: "split", dir: "col", children: [
      { id: "top", kind: "split", dir: "row", children: [group("g-side", homes.side, 232), { id: "viewer", kind: "viewer" }, group("g-right", homes.right, 288)] },
      group("g-bottom", homes.bottom, 300),
    ],
  });
}

let seq = 0;
const newId = (prefix: string) => `${prefix}-${Date.now().toString(36)}${(seq++).toString(36)}`;

export function* walk(n: Node): Generator<Node> {
  yield n;
  if (n.kind === "split") for (const c of n.children) yield* walk(c);
}

export const find = (root: Node, id: string): Node | undefined => [...walk(root)].find((n) => n.id === id);

export const groupOf = (root: Node, panel: string): Group | undefined =>
  [...walk(root)].find((n): n is Group => n.kind === "group" && n.tabs.includes(panel));

export const parentOf = (root: Node, id: string): Split | undefined =>
  [...walk(root)].find((n): n is Split => n.kind === "split" && n.children.some((c) => c.id === id));

export const hasViewer = (n: Node): boolean => [...walk(n)].some((x) => x.kind === "viewer");

/** Which child of a split takes the space the others leave: the one with the viewer in it.
 *  None, in a split without it: there they share. */
export const fillOf = (s: Split): Node | undefined => s.children.find(hasViewer);

/** Every panel the tree shows. */
export const panelsIn = (root: Node): string[] => [...walk(root)].flatMap((n) => (n.kind === "group" ? n.tabs : []));

/** Take `panel` out wherever it is. A group left empty goes, and so does a split left with
 *  one child, which takes its place and its size. */
export function remove(root: Node, panel: string): Node {
  const g = groupOf(root, panel);
  if (!g) return root;
  g.tabs = g.tabs.filter((t) => t !== panel);
  if (g.active === panel) g.active = g.tabs[0] ?? "";
  return normalize(root);
}

export function normalize(n: Node): Node {
  if (n.kind !== "split") return n;
  n.children = n.children.map(normalize).filter((c) => c.kind !== "group" || c.tabs.length > 0);
  // A split inside a split of the same direction is one split.
  n.children = n.children.flatMap((c) => (c.kind === "split" && c.dir === n.dir ? c.children : [c]));
  if (n.children.length === 1) return { ...n.children[0], size: n.size ?? n.children[0].size };
  return n;
}

/**
 * Put `panel` at `target`, taken from wherever it was first. `measured` is the size in
 * pixels of what is being split, so the new group can take half of it; `want` is the
 * panel's own preferred size along that axis.
 */
export function insert(root: Node, panel: string, target: Target, measured: { w: number; h: number }, want?: { w: number; h: number }): Node {
  root = remove(root, panel);
  if ("node" in target && target.zone === "center") {
    const g = find(root, target.node);
    if (g?.kind === "group") { g.tabs.push(panel); g.active = panel; return root; }
    return root;
  }
  const zone = "root" in target ? target.root : (target.zone as Exclude<Zone, "center">);
  const dir: Dir = zone === "left" || zone === "right" ? "row" : "col";
  const before = zone === "left" || zone === "top";
  const along = dir === "row" ? measured.w : measured.h;
  const size = Math.round(Math.max(160, Math.min((dir === "row" ? want?.w : want?.h) ?? along / 2, along / 2)));
  const group: Group = { id: newId("g"), kind: "group", tabs: [panel], active: panel, size };
  const at = "root" in target ? root : find(root, target.node);
  if (!at) return root;
  const parent = parentOf(root, at.id);
  // Beside a sibling in a split that already runs this way: one more child, no new level.
  if (parent && parent.dir === dir) {
    const i = parent.children.indexOf(at);
    parent.children.splice(before ? i : i + 1, 0, group);
    return normalize(root);
  }
  const wrap: Split = { id: newId("s"), kind: "split", dir, children: before ? [group, at] : [at, group], size: at.size };
  // What is split gives up what the new group takes, and keeps the rest.
  at.size = Math.max(120, Math.round(along - size));
  if (!parent) return normalize(wrap);
  parent.children[parent.children.indexOf(at)] = wrap;
  return normalize(root);
}

/** A tree read back from storage, made safe: every panel at most once, unknown ones gone,
 *  the viewer there exactly once. Anything wrong with it and it is `fallback`. */
export function sanitize(t: unknown, known: Set<string>, fallback: Node): Node {
  try {
    const seen = new Set<string>();
    let viewers = 0;
    const fix = (n: Node): Node | null => {
      if (n.kind === "viewer") { viewers++; return { id: "viewer", kind: "viewer", size: n.size }; }
      if (n.kind === "group") {
        const tabs = n.tabs.filter((p) => known.has(p) && !seen.has(p) && seen.add(p));
        return tabs.length ? { ...n, tabs, active: tabs.includes(n.active) ? n.active : tabs[0] } : null;
      }
      if (n.kind === "split" && (n.dir === "row" || n.dir === "col")) {
        return { ...n, children: n.children.map(fix).filter((c): c is Node => !!c) };
      }
      return null;
    };
    const out = fix(t as Node);
    if (!out || viewers !== 1) return fallback;
    return normalize(out);
  } catch {
    return fallback;
  }
}
