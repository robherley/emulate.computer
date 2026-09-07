import { test } from "node:test";
import assert from "node:assert/strict";
import { createRestorable } from "../src/session/restore.ts";

function harness() {
  let disk = "seed";
  const events: string[] = [];
  const machines: { ram: string; disk: string; freed: boolean }[] = [];
  const driver = {
    async create() {
      events.push("create");
      const machine = { ram: "firmware", disk, freed: false };
      machines.push(machine);
      return machine;
    },
    async restore(machine: (typeof machines)[number]) {
      machine.ram = "partial snapshot";
      disk = "partial overlay";
      throw new Error("truncated snapshot");
    },
    destroy(machine: (typeof machines)[number]) {
      events.push("destroy");
      assert.equal(machine.freed, false);
      machine.freed = true;
    },
    async resetDisk() { events.push("reset"); disk = "seed"; },
    refused() { events.push("refused"); },
  };
  return { driver, events, machines };
}

test("failed restore disposes partial state and reseeds before cold boot", async () => {
  const h = harness();
  const result = await createRestorable(h.driver);
  assert.deepEqual(h.events, ["create", "destroy", "refused", "reset", "create"]);
  assert.equal(result.restored, false);
  assert.deepEqual(result.candidate, { ram: "firmware", disk: "seed", freed: false });
  assert.equal(h.machines[0].freed, true);
});

test("failed disk reset aborts fallback rather than booting an overlay", async () => {
  const h = harness();
  h.driver.resetDisk = async () => { throw new Error("disk locked"); };
  await assert.rejects(createRestorable(h.driver), /disk locked/);
  assert.equal(h.machines.length, 1);
  assert.equal(h.machines[0].freed, true);
});

for (const restored of [true, false]) {
  test(`restore result ${restored} retains the candidate without reseeding`, async () => {
    const h = harness();
    const result = await createRestorable({ ...h.driver, restore: async () => restored });
    assert.equal(result.candidate, h.machines[0]);
    assert.equal(result.restored, restored);
    assert.deepEqual(h.events, ["create"]);
  });
}
