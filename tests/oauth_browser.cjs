const http = require("http");
const crypto = require("crypto");
const { spawn } = require("child_process");
const { chromium } = require("playwright");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const USERNAME = "browser-oauth-user";
const PASSWORD = "browser-oauth-password-123";
const VERIFIER = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve(server.address().port));
  });
}

function close(server) {
  return new Promise((resolve) => server.close(() => resolve()));
}

async function freePort() {
  const server = http.createServer();
  const port = await listen(server);
  await close(server);
  return port;
}

async function waitFor(url) {
  for (let i = 0; i < 100; i += 1) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch (_) {}
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("bridge did not become ready");
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

(async () => {
  let callbackRequest;
  const callback = http.createServer((request, response) => {
    callbackRequest = new URL(request.url, `http://${request.headers.host}`);
    response.writeHead(200, { "content-type": "text/plain", "cache-control": "no-store" });
    response.end("OAuth browser regression callback reached");
  });
  const callbackPort = await listen(callback);
  const callbackUri = `http://127.0.0.1:${callbackPort}/callback`;
  const bridgePort = await freePort();
  const bridgeOrigin = `http://127.0.0.1:${bridgePort}`;

  const bridge = spawn(path.join(ROOT, "target", "debug", "mcp-bridge"), [], {
    cwd: ROOT,
    stdio: ["ignore", "ignore", "ignore"],
    env: {
      PATH: process.env.PATH,
      HOME: process.env.HOME,
      MCP_PROFILE: "server-secure",
      MCP_HOST: "127.0.0.1",
      MCP_PORT: String(bridgePort),
      BRIDGE_WORKDIR: ROOT,
      BRIDGE_BACKEND_URL: "http://127.0.0.1:9",
      MCP_STATE_FILE: ":memory:",
      MCP_PUBLIC_URL: bridgeOrigin,
      MCP_OAUTH_ALLOW_INSECURE_HTTP: "true",
      MCP_OAUTH_USERNAME: USERNAME,
      MCP_OAUTH_PASSWORD: PASSWORD,
      RUST_LOG: "error",
    },
  });

  let browser;
  try {
    await waitFor(`${bridgeOrigin}/`);
    const registrationResponse = await fetch(`${bridgeOrigin}/oauth/register`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        client_name: "Browser OAuth regression",
        redirect_uris: [callbackUri],
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
        application_type: "native",
      }),
    });
    assert(registrationResponse.status === 201, "dynamic client registration failed");
    const registration = await registrationResponse.json();
    assert(typeof registration.client_id === "string", "registration did not return client_id");

    const challenge = crypto.createHash("sha256").update(VERIFIER).digest("base64url");
    const authorize = new URL(`${bridgeOrigin}/oauth/authorize`);
    authorize.search = new URLSearchParams({
      response_type: "code",
      client_id: registration.client_id,
      redirect_uri: callbackUri,
      state: "browser-regression-state",
      code_challenge: challenge,
      code_challenge_method: "S256",
      resource: `${bridgeOrigin}/mcp`,
      scope: "mcp:tools offline_access",
    }).toString();

    browser = await chromium.launch({
      executablePath: "/usr/bin/google-chrome",
      headless: true,
      args: ["--no-sandbox", "--disable-dev-shm-usage"],
    });
    const page = await browser.newPage();
    const consoleMessages = [];
    page.on("console", (message) => consoleMessages.push(message.text()));

    const login = await page.goto(authorize.toString(), { waitUntil: "domcontentloaded" });
    assert(login && login.status() === 200, "OAuth login page did not load");
    const csp = login.headers()["content-security-policy"] || "";
    assert(csp.includes(`form-action 'self' http://127.0.0.1:${callbackPort}`), "CSP does not allow the validated callback origin");

    await page.locator('input[name="username"]').fill(USERNAME);
    await page.locator('input[name="password"]').fill(PASSWORD);
    await Promise.all([
      page.waitForURL((url) => url.origin === `http://127.0.0.1:${callbackPort}`, { timeout: 10000 }),
      page.getByRole("button", { name: "Authorize" }).click(),
    ]);

    assert(callbackRequest, "registered callback was not reached");
    assert(callbackRequest.origin === `http://127.0.0.1:${callbackPort}`, "final destination origin changed");
    assert(callbackRequest.searchParams.has("code"), "authorization response did not contain a code");
    assert(callbackRequest.searchParams.get("state") === "browser-regression-state", "OAuth state was not preserved");
    assert(!consoleMessages.some((message) => /content security policy|form-action/i.test(message)), "browser reported a CSP form-action violation");
    assert(!consoleMessages.some((message) => message.includes(PASSWORD)), "password appeared in browser console output");
    assert(!consoleMessages.some((message) => /mcp_(code|access|refresh)_/i.test(message)), "OAuth secret appeared in browser console output");

    console.log("OAuth browser regression passed");
  } finally {
    if (browser) await browser.close();
    bridge.kill("SIGTERM");
    await close(callback);
  }
})().catch((error) => {
  console.error(`OAuth browser regression failed: ${error.message}`);
  process.exitCode = 1;
});
