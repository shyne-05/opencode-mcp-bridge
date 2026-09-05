const assert = require("node:assert/strict");
const path = require("node:path");
const os = require("node:os");
const { mkdtemp, writeFile, rm } = require("node:fs/promises");
const { spawn } = require("node:child_process");
const readline = require("node:readline");

const HELPER_PROTOCOL = "mcp-browser-helper/2";
const helper = path.join(__dirname, "..", "scripts", "browser.cjs");

async function nextLine(iterator, timeoutMs = 5000) {
  let timer;
  try {
    return await Promise.race([
      iterator.next().then(({ value, done }) => {
        if (done) throw new Error("browser worker stdout closed unexpectedly");
        return value;
      }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("timed out waiting for browser worker response")), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function withWorker(test, preload) {
  const worker = spawn(process.execPath, [
    ...(preload ? ["--require", preload] : []), helper, "serve",
  ], {
    env: process.env,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  worker.stderr.setEncoding("utf8");
  worker.stderr.on("data", (chunk) => { stderr += chunk; });
  const lines = readline.createInterface({ input: worker.stdout, crlfDelay: Infinity });
  const iterator = lines[Symbol.asyncIterator]();
  let nextId = 1;
  async function raw(line) {
    worker.stdin.write(`${line}\n`);
    return JSON.parse(await nextLine(iterator));
  }
  async function request(action, targetId = "", args = []) {
    const id = nextId++;
    const response = await raw(JSON.stringify({ id, action, targetId, args }));
    assert.equal(response.id, id, "worker response should match its request");
    return response;
  }

  try {
    assert.deepEqual(JSON.parse(await nextLine(iterator)), { type: "ready", protocol: HELPER_PROTOCOL });
    await test({ worker, raw, request });
  } finally {
    lines.close();
    await new Promise((resolve) => {
      if (worker.exitCode !== null || worker.signalCode !== null) return resolve();
      const timer = setTimeout(() => {
        worker.kill("SIGKILL");
        resolve();
      }, 2000);
      worker.once("exit", () => { clearTimeout(timer); resolve(); });
      worker.kill("SIGTERM");
    });
  }
  if (stderr.trim()) throw new Error(`browser worker wrote unexpected stderr: ${stderr.trim()}`);
}

// Exercise actual helper requests without attaching to the user's Chrome session.
const mockPlaywright = `
const Module = require("node:module");
const originalLoad = Module._load;
const stats = { connections: 0, sessions: 0, targetQueries: 0, detaches: 0 };
let failLookup = false;
const chromium = {
  async connectOverCDP() {
    stats.connections++;
    let connected = true;
    const pages = [];
    const context = {
      pages: () => pages,
      async newCDPSession(page) {
        stats.sessions++;
        return {
          async send() {
            stats.targetQueries++;
            if (failLookup) { failLookup = false; throw new Error("temporary CDP failure"); }
            return { targetInfo: { targetId: page.id } };
          },
          async detach() { stats.detaches++; },
        };
      },
    };
    function makePage(id) {
      let url = "about:blank";
      return {
        id,
        context: () => context,
        async goto(nextUrl) { url = nextUrl; },
        url: () => url,
        async evaluate(expression) {
          if (expression === "add-page") {
            pages.push(makePage("target4"));
            failLookup = true;
          }
          if (expression === "disconnect") connected = false;
          return { ...stats };
        },
        locator() {
          return {
            first() { return this; },
            async ariaSnapshot() { return "x".repeat(20000); },
            async click() { throw new Error(String.fromCharCode(0).repeat(20000)); },
          };
        },
      };
    }
    pages.push(...["target1", "target2", "target3"].map(makePage));
    return { isConnected: () => connected, contexts: () => [context] };
  },
};
Module._load = function (id, ...args) {
  return id === "playwright" ? { chromium } : originalLoad.call(this, id, ...args);
};
`;

async function main() {
  await withWorker(async ({ worker, raw, request }) => {
    for (const line of ["{invalid", "null", "[]", "42", '"text"']) {
      const error = await raw(line);
      assert.equal(error.id, null);
      assert.equal(error.ok, false);
      assert.match(error.error, /invalid worker request/);
      const ping = await request("ping");
      assert.equal(ping.ok, true, "malformed input must not kill the worker");
      assert.equal(ping.result, HELPER_PROTOCOL);
    }
    assert.equal(worker.exitCode, null, "worker should remain alive across requests");
  });

  const temporary = await mkdtemp(path.join(os.tmpdir(), "mcp-browser-worker-test-"));
  try {
    const preload = path.join(temporary, "mock-playwright.cjs");
    await writeFile(preload, mockPlaywright);
    await withWorker(async ({ request }) => {
      async function metrics() {
        const response = await request("evaluate", "target3", ["stats"]);
        assert.equal(response.ok, true);
        return JSON.parse(response.result);
      }
      assert.deepEqual(await metrics(), { connections: 1, sessions: 3, targetQueries: 3, detaches: 3 });
      for (let i = 0; i < 5; i++) {
        const navigation = await request("navigate", "target3", [`https://example.test/page${i}`]);
        assert.equal(navigation.ok, true);
        assert.equal(navigation.result, `navigated target3 to https://example.test/page${i}`);
        assert.equal((await metrics()).targetQueries, 3, "cached pages should require no additional CDP sessions");
      }
      const missing = await request("evaluate", "missing", ["stats"]);
      assert.equal(missing.ok, false);
      assert.match(missing.error, /browser target not found/);

      await request("evaluate", "target3", ["add-page"]);
      const failed = await request("evaluate", "target4", ["stats"]);
      assert.equal(failed.ok, false);
      assert.match(failed.error, /temporary CDP failure/);
      const retry = await request("evaluate", "target4", ["stats"]);
      assert.equal(retry.ok, true, "a failed target lookup must be retryable");
      assert.deepEqual(JSON.parse(retry.result), { connections: 1, sessions: 5, targetQueries: 5, detaches: 5 });

      const snapshot = await request("snapshot", "target3");
      assert.equal(snapshot.result.length, 15000);
      const failedClick = await request("click", "target3", ["button"]);
      assert.equal(failedClick.ok, false);
      assert.equal(failedClick.error.length, 15000);
      assert.ok(Buffer.byteLength(JSON.stringify(failedClick)) < 128 * 1024, "escaped output must fit Rust's frame limit");

      await request("evaluate", "target3", ["disconnect"]);
      assert.deepEqual(await metrics(), { connections: 2, sessions: 8, targetQueries: 8, detaches: 8 });
    }, preload);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
  console.log("browser worker protocol, target-cache, and output-limit regressions passed");
}

main().catch((error) => {
  console.error(error.stack || error.message);
  process.exit(1);
});
