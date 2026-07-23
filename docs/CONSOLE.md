# Reliability Console

The FamilyClaw Reliability Console is the gateway's single-page operator view
at `GET /console`. The design goal is Hermes-class live visibility: at every
moment the sticky **Now** strip answers *"what is the agent doing?"* — thinking,
running a tool, or waiting for your approval — while the activity feed streams
redacted turn-audit events and pending approvals sit one click away.

> Screenshot placeholder: Reliability Console Now strip + activity feed +
> pending approvals.

## Authentication

The HTML shell is public (no secrets). Protected data arrives only through
`/console/events` and the approval/audit JSON routes, using the same optional
bearer token as other operator endpoints. When `FAMILYCLAW_GATEWAY_TOKEN` is
configured, bootstrap the browser session with:

```text
http://127.0.0.1:8787/console?token=YOUR_TOKEN
```

The page stores the token in browser local storage for its API requests. Use
the **token** button in the header to replace or clear it. Do not share a
console URL containing a token; after the page loads, it removes the query
parameter from the displayed address.

With no configured gateway token, the console works without credentials on the
gateway's default loopback binding. Existing optional operator ACL checks still
apply to approval API requests.

## Live events

`GET /console/events` is a Server-Sent Events endpoint. The gateway polls the
same in-memory redacted audit collector as `GET /turns/audit` once per second,
then sends only events not yet delivered to that connection. Browser
`EventSource` reconnects automatically; the console status indicator reflects
the connection state.

The event endpoint accepts the bearer token as `?token=` because browser
`EventSource` cannot attach an `Authorization` header. The gateway compares
the token in constant time and never logs it. Event payloads are the existing
redacted audit records: action identifier, event kind, timestamp, and
redacted detail. Raw tool arguments, payloads, and secrets are never sent to
the console.
