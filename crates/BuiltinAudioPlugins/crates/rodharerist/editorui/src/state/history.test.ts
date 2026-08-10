import { describe, expect, test } from "bun:test";
import {
  HISTORY_LIMIT,
  activeBankSlot,
  activeSnapshot,
  canRedo,
  canUndo,
  commit,
  copyToOther,
  createAb,
  createHistory,
  redo,
  reset,
  saveBankSlot,
  setActiveBankSlot,
  switchSlot,
  undo,
  type BankState,
} from "./history";

describe("undo/redo history", () => {
  test("a fresh history has nothing to undo or redo", () => {
    const h = createHistory("a");
    expect(canUndo(h)).toBe(false);
    expect(canRedo(h)).toBe(false);
    expect(h.present).toBe("a");
  });

  test("undo walks back and redo walks forward through commits", () => {
    let h = createHistory("a");
    h = commit(h, "b");
    h = commit(h, "c");
    expect(h.present).toBe("c");

    h = undo(h);
    expect(h.present).toBe("b");
    h = undo(h);
    expect(h.present).toBe("a");
    expect(canUndo(h)).toBe(false);

    h = redo(h);
    expect(h.present).toBe("b");
    h = redo(h);
    expect(h.present).toBe("c");
    expect(canRedo(h)).toBe(false);
  });

  test("undo and redo at the ends are no-ops rather than errors", () => {
    const h = createHistory("a");
    expect(undo(h)).toBe(h);
    expect(redo(h)).toBe(h);
  });

  test("committing after an undo discards the redo branch", () => {
    let h = createHistory("a");
    h = commit(h, "b");
    h = undo(h);
    expect(canRedo(h)).toBe(true);

    h = commit(h, "z");
    expect(canRedo(h)).toBe(false);
    expect(h.present).toBe("z");
    expect(h.past).toEqual(["a"]);
  });

  test("an equal commit is collapsed so undo never becomes a no-op step", () => {
    const eq = (a: string, b: string) => a === b;
    let h = createHistory("a");
    h = commit(h, "a", eq);
    expect(canUndo(h)).toBe(false);

    h = commit(h, "b", eq);
    expect(canUndo(h)).toBe(true);
  });

  test("history is bounded, dropping the oldest entries", () => {
    let h = createHistory(0);
    for (let i = 1; i <= HISTORY_LIMIT + 10; i++) h = commit(h, i);
    expect(h.past.length).toBe(HISTORY_LIMIT);
    // 74 commits leave 74 past entries; keeping the last 64 starts at 10.
    expect(h.past[0]).toBe(10);
    expect(h.present).toBe(HISTORY_LIMIT + 10);
  });

  test("reset establishes a new baseline with no undo step", () => {
    let h = createHistory("a");
    h = commit(h, "b");
    h = reset(h, "preset-2");
    expect(h.present).toBe("preset-2");
    expect(canUndo(h)).toBe(false);
    expect(canRedo(h)).toBe(false);
  });

  test("history operations do not mutate the previous history", () => {
    const h = createHistory("a");
    const next = commit(h, "b");
    expect(h.present).toBe("a");
    expect(h.past).toEqual([]);
    expect(next.present).toBe("b");
  });
});

describe("A/B compare", () => {
  test("both slots start from the loaded state and A is active", () => {
    const ab = createAb("rig");
    expect(ab.active).toBe("A");
    expect(activeSnapshot(ab)).toBe("rig");
    expect(ab.B).toBe("rig");
  });

  test("switching stores current edits into the outgoing slot", () => {
    let ab = createAb("base");
    // Edited A into "a-edit", then switched to B.
    ab = switchSlot(ab, "a-edit");
    expect(ab.active).toBe("B");
    expect(ab.A).toBe("a-edit");
    expect(activeSnapshot(ab)).toBe("base");

    // Edit B, switch back: A's edit is intact and B's is retained.
    ab = switchSlot(ab, "b-edit");
    expect(ab.active).toBe("A");
    expect(activeSnapshot(ab)).toBe("a-edit");
    expect(ab.B).toBe("b-edit");
  });

  test("copying the active slot overwrites only the other slot", () => {
    let ab = createAb("base");
    ab = copyToOther(ab, "a-edit");
    expect(ab.active).toBe("A");
    expect(ab.A).toBe("a-edit");
    expect(ab.B).toBe("a-edit");
  });

  test("copy from B writes into A", () => {
    let ab = createAb("base");
    ab = switchSlot(ab, "a-edit");
    ab = copyToOther(ab, "b-edit");
    expect(ab.active).toBe("B");
    expect(ab.B).toBe("b-edit");
    expect(ab.A).toBe("b-edit");
  });
});

describe("snapshot bank", () => {
  const bank: BankState<string> = { active: 0, slots: ["one", "two", "three"] };

  test("switching slots never touches stored contents (unlike A/B, a live edit is discarded, not synced)", () => {
    const next = setActiveBankSlot(bank, 2);
    expect(next.active).toBe(2);
    expect(activeBankSlot(next)).toBe("three");
    // The slot we switched away from is untouched — no "current" was ever
    // passed in to sync, matching a real footswitch bank.
    expect(next.slots).toEqual(["one", "two", "three"]);
  });

  test("switching to the already-active slot or an out-of-range index is a no-op", () => {
    expect(setActiveBankSlot(bank, 0)).toBe(bank);
    expect(setActiveBankSlot(bank, -1)).toBe(bank);
    expect(setActiveBankSlot(bank, 3)).toBe(bank);
  });

  test("saving overwrites exactly one slot and leaves the rest and the active index alone", () => {
    const next = saveBankSlot(bank, 1, "two-edited");
    expect(next.active).toBe(0);
    expect(next.slots).toEqual(["one", "two-edited", "three"]);
    // Original is untouched.
    expect(bank.slots).toEqual(["one", "two", "three"]);
  });

  test("saving to an out-of-range index is a no-op", () => {
    expect(saveBankSlot(bank, 9, "x")).toBe(bank);
  });
});
