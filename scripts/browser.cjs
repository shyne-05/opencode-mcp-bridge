const HELPER_PROTOCOL = "mcp-browser-helper/2";
const CDP_ENDPOINT = "http://127.0.0.1:9222";

const action = process.argv[2];
if (action === "version") {
  console.log(HELPER_PROTOCOL);
  process.exit(0);
}

const { chromium } = require("playwright");
const readline = require("node:readline");

let browserPromise = null;

async function browserInstance() {
  if (browserPromise) {
    const current = await browserPromise;
    if (current.isConnected()) return current;
    browserPromise = null;
  }
  browserPromise = chromium.connectOverCDP(CDP_ENDPOINT).catch((error) => {
    browserPromise = null;
    throw error;
  });
  return browserPromise;
}

async function browserContext() {
  const browser = await browserInstance();
  const context = browser.contexts()[0];
  if (!context) throw new Error("Chrome has no browser context");
  return context;
}

async function pageTargetId(page) {
  const session = await page.context().newCDPSession(page);
  try {
    const { targetInfo } = await session.send("Target.getTargetInfo");
    return targetInfo.targetId;
  } finally {
    await session.detach();
  }
}

async function selectPage(context, requestedTargetId) {
  const pages = context.pages();
  if (requestedTargetId) {
    for (const page of pages) {
      if ((await pageTargetId(page)) === requestedTargetId) return page;
    }
    throw new Error(`browser target not found: ${requestedTargetId}`);
  }
  return pages.at(-1) || (await context.newPage());
}

function printableResult(result) {
  if (typeof result === "string") return result;
  if (result === undefined) return "undefined";
  try {
    const json = JSON.stringify(result);
    return json === undefined ? String(result) : json;
  } catch (error) {
    return JSON.stringify({
      type: typeof result,
      value: String(result),
      serializationError: error.message,
    });
  }
}

async function performAction(requestedAction, targetId, args) {
  if (requestedAction === "ping") return HELPER_PROTOCOL;

  const context = await browserContext();
  const page = await selectPage(context, targetId);

  if (requestedAction === "snapshot") {
    const body = page.locator("body");
    const snapshot = typeof body.ariaSnapshot === "function"
      ? await body.ariaSnapshot({ timeout: 10000 })
      : await body.innerText({ timeout: 10000 });
    return String(snapshot).slice(0, 15000);
  }
  if (requestedAction === "navigate") {
    const url = args[0];
    if (!url) throw new Error("url is required");
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 15000 });
    return `navigated ${await pageTargetId(page)} to ${page.url()}`;
  }
  if (requestedAction === "click") {
    const selector = args[0];
    if (!selector) throw new Error("selector is required");
    await page.locator(selector).first().click({ timeout: 10000 });
    return `clicked ${selector}`;
  }
  if (requestedAction === "fill") {
    const selector = args[0];
    if (!selector) throw new Error("selector is required");
    await page.locator(selector).first().fill(args[1] || "", { timeout: 10000 });
    return `filled ${selector}`;
  }
  if (requestedAction === "evaluate") {
    const expression = args[0];
    if (!expression) throw new Error("expression is required");
    const result = await page.evaluate(expression);
    return printableResult(result).slice(0, 5000);
  }
  throw new Error(`unsupported browser script action: ${requestedAction}`);
}

async function serve() {
  process.stdout.write(`${JSON.stringify({ type: "ready", protocol: HELPER_PROTOCOL })}\n`);
  const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

  for await (const line of input) {
    if (!line.trim()) continue;
    let request;
    try {
      request = JSON.parse(line);
    } catch (error) {
      process.stdout.write(`${JSON.stringify({ id: null, ok: false, error: `invalid worker request: ${error.message}` })}\n`);
      continue;
    }

    const id = request.id ?? null;
    try {
      const result = await performAction(
        String(request.action || ""),
        typeof request.targetId === "string" ? request.targetId : "",
        Array.isArray(request.args) ? request.args.map(String) : [],
      );
      process.stdout.write(`${JSON.stringify({ id, ok: true, result })}\n`);
    } catch (error) {
      process.stdout.write(`${JSON.stringify({ id, ok: false, error: error.message })}\n`);
    }
  }
}

async function main() {
  if (action === "serve") {
    await serve();
    return;
  }

  // v0.4 convention: action, targetId, args...
  // Backward-compatible with v0.3: action, args... (no targetId slot).
  const candidate = process.argv[3];
  const looksLikeTargetId = typeof candidate === "string" && /^[A-Fa-f0-9]{32}$/.test(candidate);
  const hasV4TargetSlot = candidate === "" || looksLikeTargetId;
  const targetId = looksLikeTargetId ? candidate : "";
  const args = process.argv.slice(hasV4TargetSlot ? 4 : 3);
  const result = await performAction(action, targetId, args);
  console.log(result);
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
