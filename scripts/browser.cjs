const { chromium } = require("playwright");

const action = process.argv[2];
const args = process.argv.slice(3);

async function main() {
  const browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
  const context = browser.contexts()[0];
  if (!context) throw new Error("Chrome has no browser context");

  let page = context.pages()[0];
  if (!page) page = await context.newPage();

  if (action === "snapshot") {
    console.log(JSON.stringify(await page.accessibility.snapshot()).slice(0, 15000));
  } else if (action === "click") {
    await page.click(args[0]);
    console.log(`clicked ${args[0]}`);
  } else if (action === "fill") {
    await page.fill(args[0], args[1] || "");
    console.log("filled");
  } else if (action === "evaluate") {
    const result = await page.evaluate(args[0]);
    console.log(String(result).slice(0, 5000));
  } else {
    throw new Error(`unsupported browser script action: ${action}`);
  }

  await browser.close();
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
