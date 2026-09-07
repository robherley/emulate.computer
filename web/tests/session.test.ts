import { test } from "node:test";
import assert from "node:assert/strict";
import { Session, type SessionState } from "../src/session/session.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function harness(
  create = async (name: string, _disk: unknown, _snapshot: boolean) => name,
) {
  const events: string[] = [];
  const states: SessionState[] = [];
  const disk = {
    reset: async () => {
      events.push("reset-disk");
    },
    dispose: async () => {
      events.push("dispose-disk");
    },
  };
  const session = new Session(disk, {
    create: async (name: string, disk: unknown, snapshot: boolean) => {
      events.push(`create:${name}:${snapshot}`);
      return create(name, disk, snapshot);
    },
    destroy: (name: string) => {
      events.push(`destroy:${name}`);
    },
    started: (name: string) => {
      events.push(`started:${name}`);
    },
    pause: () => {
      events.push("pause");
    },
    changed: (state) => {
      states.push(state);
    },
    error: (error) => {
      events.push(`error:${String(error)}`);
    },
  });
  return { session, disk, events, states };
}

test("restart cold-boots a replacement while retaining the disk", async () => {
  const h = harness();
  await h.session.boot("linux");
  assert(h.events.includes("create:linux:true"));
  h.events.length = 0;
  await h.session.restart();
  assert.deepEqual(h.events, [
    "pause",
    "destroy:linux",
    "create:linux:false",
    "started:linux",
  ]);
  assert.equal(h.session.machine, "linux");
});

test("a newer boot supersedes an in-flight candidate without starting it", async () => {
  const pending = deferred<string>();
  const h = harness(async (name) => (name === "old" ? pending.promise : name));
  const old = h.session.boot("old");
  await Promise.resolve();
  const latest = h.session.boot("new");
  assert.equal(h.session.machine, null);
  pending.resolve("old");
  await Promise.all([old, latest]);
  assert.equal(h.session.machine, "new");
  assert(!h.events.includes("started:old"));
  assert(h.events.indexOf("destroy:old") < h.events.indexOf("create:new:true"));
});

test("shutdown during startup disposes the candidate and ends stopped", async () => {
  const pending = deferred<string>();
  const h = harness(() => pending.promise);
  const boot = h.session.boot("linux");
  await Promise.resolve();
  const stop = h.session.stop();
  pending.resolve("linux");
  await Promise.all([boot, stop]);
  assert.equal(h.session.state, "stopped");
  assert.equal(h.session.machine, null);
  assert(!h.events.includes("started:linux"));
  assert.equal(h.events.filter((event) => event === "destroy:linux").length, 1);
});

test("disk reset releases its owner before erasing and restarting", async () => {
  const h = harness();
  await h.session.boot("linux");
  h.events.length = 0;
  await h.session.reset();
  assert.deepEqual(h.events, [
    "pause",
    "destroy:linux",
    "reset-disk",
    "create:linux:true",
    "started:linux",
  ]);
});

test("a reset failure is visible and does not boot against the disk that could not be erased", async () => {
  const h = harness();
  await h.session.boot("linux");
  h.disk.reset = async () => {
    throw new Error("disk locked");
  };
  h.events.length = 0;
  await h.session.reset();
  assert.equal(h.session.state, "failed");
  assert.equal(h.session.machine, null);
  assert(h.events.includes("error:Error: disk locked"));
  assert(!h.events.some((event) => event.startsWith("create:")));
});

test("failure is recoverable through a new boot", async () => {
  const h = harness(async (name) => {
    if (name === "broken") throw new Error("bad image");
    return name;
  });
  await h.session.boot("broken");
  assert.equal(h.session.state, "failed");
  await h.session.boot("fixed");
  assert.equal(h.session.state, "running");
  assert.equal(h.session.machine, "fixed");
});

test("a rejected stale boot cannot fail the newer boot", async () => {
  const pending = deferred<string>();
  const h = harness(async (name) => (name === "old" ? pending.promise : name));
  const old = h.session.boot("old");
  await Promise.resolve();
  const latest = h.session.boot("new");
  pending.reject(new Error("old failed"));
  await Promise.all([old, latest]);
  assert.equal(h.session.state, "running");
  assert(!h.events.some((event) => event.startsWith("error:")));
});

test("runtime failure releases the machine and permits restart", async () => {
  const h = harness();
  await h.session.boot("linux");
  await h.session.fail(new Error("trap"));
  assert.equal(h.session.state, "failed");
  assert.equal(h.session.machine, null);
  await h.session.restart();
  assert.equal(h.session.state, "running");
});

test("disposing during boot closes the candidate, removes the disk and refuses later commands", async () => {
  const pending = deferred<string>();
  const h = harness(() => pending.promise);
  const boot = h.session.boot("linux");
  await Promise.resolve();
  const dispose = h.session.dispose();
  const restart = h.session.restart();
  pending.resolve("linux");
  await Promise.all([boot, dispose, restart]);
  assert.equal(h.session.state, "disposed");
  assert.deepEqual(
    h.events.filter((event) => !event.startsWith("pause")),
    ["create:linux:true", "destroy:linux", "dispose-disk"],
  );
});
