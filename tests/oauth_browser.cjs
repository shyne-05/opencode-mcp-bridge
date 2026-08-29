const http = require("http");
const crypto = require("crypto");
const { spawn } = require("child_process");
const { chromium } = require("playwright");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const USERNAME = "browser-oauth-user";
const PASSWORD = "browser-oauth-password-123";
const VERIFIER = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~";
const COOKIE_SECRET = "browser-oauth-cookie-canary-7f4c";
const API_KEY_SECRET = "browser-oauth-api-key-canary-7f4c";
const TUNNEL_TOKEN_SECRET = "browser-oauth-tunnel-token-canary-7f4c";
const PRIVATE_KEY_SECRET = "-----BEGIN PRIVATE KEY-----browser-oauth-private-key-canary-7f4c";

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

function appendBounded(current, chunk) {
  const next = current + chunk.toString();
  return next.length > 1024 * 1024 ? next.slice(-(1024 * 1024)) : next;
}

function assertSecretsNotPrinted(output, secrets) {
  for (const [name, value] of Object.entries(secrets)) {
    assert(!output.includes(value), `${name} appeared in captured output`);
  }
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
    stdio: ["ignore", "pipe", "pipe"],
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
      MCP_TEST_API_KEY: API_KEY_SECRET,
      CLOUDFLARED_TOKEN: TUNNEL_TOKEN_SECRET,
      MCP_TEST_PRIVATE_KEY: PRIVATE_KEY_SECRET,
      RUST_LOG: "error",
    },
  });
  let bridgeOutput = "";
  bridge.stdout.on("data", (chunk) => {
    bridgeOutput = appendBounded(bridgeOutput, chunk);
  });
  bridge.stderr.on("data", (chunk) => {
    bridgeOutput = appendBounded(bridgeOutput, chunk);
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
    await page.context().addCookies([
      { name: "oauth_regression_cookie", value: COOKIE_SECRET, url: bridgeOrigin },
    ]);
    const consoleMessages = [];
    page.on("console", (message) => consoleMessages.push(message.text()));

    const login = await page.goto(authorize.toString(), { waitUntil: "domcontentloaded" });
    assert(login && login.status() === 200, "OAuth login page did not load");
    const csp = login.headers()["content-security-policy"] || "";
    const formAction = csp
      .split(";")
      .map((directive) => directive.trim())
      .find((directive) => directive.startsWith("form-action "));
    assert(
      formAction === `form-action 'self' http://127.0.0.1:${callbackPort}`,
      "CSP form-action is not limited to self and the validated callback origin",
    );
    assert(!csp.includes("*"), "CSP contains a wildcard source");

    assert(
      (await page.locator('input[name="username"]').inputValue()) === USERNAME,
      "OAuth login did not prefill the configured username",
    );
    await page.locator('input[name="username"]').fill(USERNAME);
    await page.locator('input[name="password"]').fill(PASSWORD);
    const [authorizePost] = await Promise.all([
      page.waitForResponse(
        (response) =>
          response.url() === `${bridgeOrigin}/oauth/authorize` &&
          response.request().method() === "POST",
        { timeout: 10000 },
      ),
      page.waitForURL((url) => url.origin === `http://127.0.0.1:${callbackPort}`, {
        timeout: 10000,
      }),
      page.getByRole("button", { name: "Authorize" }).click(),
    ]);
    assert(authorizePost.status() === 302, "successful OAuth login POST did not return 302");

    assert(callbackRequest, "registered callback was not reached");
    assert(callbackRequest.origin === `http://127.0.0.1:${callbackPort}`, "final destination origin changed");
    const authorizationCode = callbackRequest.searchParams.get("code");
    assert(authorizationCode, "authorization response did not contain a code");
    assert(callbackRequest.searchParams.get("state") === "browser-regression-state", "OAuth state was not preserved");
    assert(!consoleMessages.some((message) => /content security policy|form-action/i.test(message)), "browser reported a CSP form-action violation");

    const tokenResponse = await fetch(`${bridgeOrigin}/oauth/token`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "authorization_code",
        code: authorizationCode,
        redirect_uri: callbackUri,
        client_id: registration.client_id,
        code_verifier: VERIFIER,
        resource: `${bridgeOrigin}/mcp`,
      }),
    });
    assert(tokenResponse.status === 200, "authorization code exchange failed");
    const tokens = await tokenResponse.json();
    assert(typeof tokens.access_token === "string", "token response did not contain access_token");
    assert(typeof tokens.refresh_token === "string", "token response did not contain refresh_token");

    await new Promise((resolve) => setTimeout(resolve, 50));
    const capturedOutput = `${bridgeOutput}\n${consoleMessages.join("\n")}`;
    assertSecretsNotPrinted(capturedOutput, {
      password: PASSWORD,
      authorization_code: authorizationCode,
      access_token: tokens.access_token,
      refresh_token: tokens.refresh_token,
      cookie: COOKIE_SECRET,
      api_key: API_KEY_SECRET,
      tunnel_token: TUNNEL_TOKEN_SECRET,
      private_key: PRIVATE_KEY_SECRET,
    });

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
