# Private dashboard

Structurely can run a private browser console for index health, search,
research, impact analysis, path tracing, workspaces, sessions, recaps, and
memory. The console is a static shell. Every result comes directly from a
loopback-only bridge on the same computer as the repository.

Vercel and Cloudflare receive the HTML, CSS, JavaScript, and security-header
configuration required to render the shell. They do not receive repository
source, graph or content indexes, queries, session history, recaps, memories,
pairing codes, or bearer tokens.

## Start locally

Initialize the project, then start the foreground bridge:

```bash
structurely dashboard serve --path /absolute/project
```

Open `http://127.0.0.1:4765`, choose **Connect bridge**, and enter the
eight-digit pairing code printed by the command. The code can be used once.
The resulting 256-bit token is kept in browser `sessionStorage`, so closing the
tab discards it. Only the loopback URL is kept in `localStorage`.

Use port `0` to select an available port:

```bash
structurely dashboard serve --path /absolute/project --port 0
```

The command prints the selected address and pairing code as JSON.

## Deploy the static shell

Install and authenticate the provider CLI yourself. Structurely never runs
`npm install`, changes provider credentials, or deploys without explicit
consent. The implementation follows the official
[Vercel CLI deployment](https://vercel.com/docs/cli/deploy) and
[Cloudflare Pages Direct Upload](https://developers.cloudflare.com/pages/get-started/direct-upload/)
flows.

```bash
structurely dashboard deploy vercel
structurely dashboard deploy cloudflare
```

The default provider project is `structurely-dashboard`. Override it with
`--project-name <name>`. Structurely exports into a fresh temporary directory,
invokes `vercel deploy --prod` or `wrangler pages deploy`, verifies the
reported HTTPS URL, and removes the temporary directory. The JSON report sets
`data_uploaded` to `false`; no Structurely project data is included in the
deployment.

Allow the exact deployed origin when starting the bridge:

```bash
structurely dashboard serve \
  --path /absolute/project \
  --allow-origin https://your-dashboard.example
```

Origins must be exact HTTPS origins without paths, credentials, queries, or
fragments. Remote plain HTTP origins are rejected. Local development origins
may use `http://127.0.0.1:<port>` or `http://localhost:<port>`.

To inspect the shell before deploying it:

```bash
structurely dashboard export /empty/output/directory
```

The export contains `index.html`, `app.js`, `styles.css`, `_headers`, and
`vercel.json`.

## Browser local-network permission

A dashboard loaded over HTTPS must receive browser permission before it can
call an HTTP loopback service. Accept the browser's local-network or
loopback-network prompt. Structurely responds to the browser's private-network
preflight, but only for an origin explicitly listed with `--allow-origin`.
See MDN's [local network access security
model](https://developer.mozilla.org/en-US/docs/Web/Security/Defenses/Local_network_access)
for current browser behavior.

If the browser blocks the request before showing a prompt:

1. confirm the bridge with `structurely dashboard status --path <project>`;
2. confirm the dashboard's exact origin is present in `allowed_origins`;
3. open the browser's site permissions and allow local-network access;
4. use `structurely dashboard reconnect` to issue a fresh pairing code.

## Pairing lifecycle

```bash
structurely dashboard status --path /absolute/project
structurely dashboard rotate-token --path /absolute/project
structurely dashboard reconnect --path /absolute/project
structurely dashboard stop --path /absolute/project
structurely dashboard remove --path /absolute/project
```

`status` probes the recorded loopback address and shows the unused pairing
code. `rotate-token` and `reconnect` immediately invalidate all existing tabs,
generate a new bearer token, and issue a new one-time code. `stop` requests a
bounded graceful shutdown. `remove` also deletes local dashboard control
files. Neither command removes project indexes or asks a provider to delete a
remote deployment.

Delete a hosted shell through its provider account when it is no longer
needed. It contains no project data, but removing unused public surfaces is
still good operational hygiene.

## Security boundary

The bridge has these enforced properties:

- it binds an IPv4 loopback address selected by Structurely, never a remote
  interface;
- repository APIs require a random bearer token obtained through a one-time
  pairing code;
- origin comparison is exact and timing-resistant;
- cross-origin preflights are denied unless the origin was explicitly allowed;
- request bodies are limited to 64 KiB and engine-level query, result,
  traversal, file, and payload bounds still apply;
- responses disable caching, sniffing, framing, and referrer disclosure;
- dashboard state uses owner-only permissions on Unix;
- exported provider assets contain no project path, token, pairing code,
  query, result, or index data.

The bridge is intentionally foreground-bound. Stop it when the dashboard is
not in use. Anyone with local access equivalent to the Structurely process may
already be able to read the selected repository, so operating-system account
and filesystem permissions remain part of the trust boundary.

## Installer behavior

Interactive installers offer Vercel, Cloudflare Pages, local-only, or skip
after the verified binary is installed. Redirected input and CI never prompt.
Automation can set `STRUCTURELY_DASHBOARD_SETUP` to `vercel`, `cloudflare`,
`local`, `skip`, or `prompt`.

An optional deployment failure never rolls back a verified Structurely binary
or a successful project setup. The message includes the exact command to retry.

## Troubleshooting

`origin is not allowed`
: Restart the bridge with the exact origin shown in the browser address bar.
  Do not include a trailing slash or path.

`pairing code was already used`
: Run `structurely dashboard reconnect --path <project>` and pair again.

`pairing is required`
: Pair the tab again. Tokens intentionally do not survive a closed tab or token
  rotation.

The provider CLI is missing or unusable
: Install the official Vercel or Wrangler CLI, authenticate it directly, and
  rerun `structurely dashboard deploy <provider>`.

The deployed URL works but project data is empty
: Start the local bridge, allow that deployed origin, grant browser
  local-network permission, and pair the tab. The hosted shell has no stored
  fallback data by design.
