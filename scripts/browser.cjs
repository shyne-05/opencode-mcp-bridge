const HELPER_PROTOCOL = "mcp-browser-helper/2";

const action = process.argv[2];
if (action === "version") {
  console.log(HELPER_PROTOCOL);
  process.exit(0);
}

const { chromium } = require("playwright");

// v0.4 convention: action, targetId, args...
// Backward-compatible with v0.3: action, args... (no targetId slot).
const candidate = process.argv[3];
const looksLikeTargetId = typeof candidate === "string" && /^[A-Fa-f0-9]{32}$/.test(candidate);
const hasV4TargetSlot = candidate === "" || looksLikeTargetId;
const targetId = looksLikeTargetId ? candidate : "";
const args = process.argv.slice(hasV4TargetSlot ? 4 : 3);

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
    return JSON.stringify({ type: typeof result, value: String(result), serializationError: error.message });
  }
}

async function main() {
  const browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
  const context = browser.contexts()[0];
  if (!context) throw new Error("Chrome has no browser context");
  const page = await selectPage(context, targetId);

  if (action === "snapshot") {
    const body = page.locator("body");
    const snapshot = typeof body.ariaSnapshot === "function"
      ? await body.ariaSnapshot({ timeout: 10000 })
      : await body.innerText({ timeout: 10000 });
    console.log(String(snapshot).slice(0, 15000));
  } else if (action === "navigate") {
    const url = args[0];
    if (!url) throw new Error("url is required");
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 15000 });
    console.log(`navigated ${await pageTargetId(page)} to ${page.url()}`);
  } else if (action === "click") {
    const selector = args[0];
    if (!selector) throw new Error("selector is required");
    await page.locator(selector).first().click({ timeout: 10000 });
    console.log(`clicked ${selector}`);
  } else if (action === "fill") {
    const selector = args[0];
    if (!selector) throw new Error("selector is required");
    await page.locator(selector).first().fill(args[1] || "", { timeout: 10000 });
    console.log(`filled ${selector}`);
  } else if (action === "evaluate") {
    const expression = args[0];
    if (!expression) throw new Error("expression is required");
    const result = await page.evaluate(expression);
    console.log(printableResult(result).slice(0, 5000));
  } else {
    throw new Error(`unsupported browser script action: ${action}`);
  }
  process.exit(0);
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
