const assert = require("node:assert/strict");
const path = require("node:path");
const { spawn } = require("node:child_process");
const readline = require("node:readline");

const HELPER_PROTOCOL = "mcp-browser-helper/2";
const helper = path.join(__dirname, "..", "scripts", "browser.cjs");

function nextLine(iterator, timeoutMs = 5000) {
  return Promise.race([
    iterator.next().then(({ value, done }) => {
      if (done) throw new Error("browser worker stdout closed unexpectedly");
      return value;
    }),
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error("timed out waiting for browser worker response")), timeoutMs),
    ),
  ]);
}

async function main() {
  const worker = spawn(process.execPath, [helper, "serve"], {
    env: process.env,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  worker.stderr.setEncoding("utf8");
  worker.stderr.on("data", (chunk) => {
    stderr += chunk;
  });

  const lines = readline.createInterface({ input: worker.stdout, crlfDelay: Infinity });
  const iterator = lines[Symbol.asyncIterator]();

  try {
    const ready = JSON.parse(await nextLine(iterator));
    assert.deepEqual(ready, { type: "ready", protocol: HELPER_PROTOCOL });

    worker.stdin.write(`${JSON.stringify({ id: 1, action: "ping", targetId: "", args: [] })}\n`);
    const first = JSON.parse(await nextLine(iterator));
    assert.deepEqual(first, { id: 1, ok: true, result: HELPER_PROTOCOL });

    worker.stdin.write(`${JSON.stringify({ id: 2, action: "ping", targetId: "", args: [] })}\n`);
    const second = JSON.parse(await nextLine(iterator));
    assert.deepEqual(second, { id: 2, ok: true, result: HELPER_PROTOCOL });

    assert.equal(worker.exitCode, null, "worker should remain alive across requests");
    console.log("browser worker protocol regression passed");
  } finally {
    lines.close();
    worker.kill("SIGTERM");
    await new Promise((resolve) => {
      if (worker.exitCode !== null) return resolve();
      worker.once("exit", resolve);
      setTimeout(() => {
        worker.kill("SIGKILL");
        resolve();
      }, 2000);
    });
  }

  if (stderr.trim()) {
    throw new Error(`browser worker wrote unexpected stderr: ${stderr.trim()}`);
  }
}

main().catch((error) => {
  console.error(error.stack || error.message);
  process.exit(1);
});
