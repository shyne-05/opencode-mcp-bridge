# Security Policy

## Reporting a vulnerability

Please report security issues privately through GitHub Security Advisories for this repository. Do not disclose tokens, cookies, private keys, tunnel credentials, or exploit details in a public issue.

## Deployment guidance

MCP Bridge can execute commands and control a browser when host tools are enabled. Run it as a dedicated, least-privileged user, keep it on loopback or a private network, use a strong token or OAuth password, and expose it publicly only through an authenticated HTTPS edge.

The built-in OAuth flow is intended for a single configured user. Use an external identity provider for multi-user or enterprise deployments. OAuth codes and tokens are held in memory and are invalidated when the bridge restarts.
