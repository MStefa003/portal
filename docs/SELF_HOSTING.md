# Running a rendezvous server

Two Portal peers need a rendezvous server to find each other. It only pairs
them and relays a few hundred bytes of handshake. File data never goes
through it, and it never learns the code words. A small VPS or a Raspberry Pi
is plenty.

There is no public instance yet. Until there is, you run one yourself and
point `portal` at it:

```console
$ portal send --rendezvous wss://portal.example.com/v1 ./project
$ portal receive --rendezvous wss://portal.example.com/v1 7-cedar-river
```

You can also set it once in the environment:

```console
$ export PORTAL_RENDEZVOUS=wss://portal.example.com/v1        # bash/zsh
PS> $env:PORTAL_RENDEZVOUS = "wss://portal.example.com/v1"     # PowerShell
```

## Quick start (LAN or testing)

```console
$ cargo run --release -p portal-rendezvous -- --listen 0.0.0.0:8080
$ portal send --rendezvous ws://192.168.1.10:8080/v1 ./project
```

Plain `ws://` is acceptable on a network you trust. Everything that matters
is protected end to end by the code handshake anyway. Over the internet,
use `wss://` so nobody on the path can see or disrupt the pairing.

## Behind a TLS reverse proxy (recommended)

The server speaks plain HTTP/WebSocket; let a reverse proxy terminate TLS.
With [Caddy](https://caddyserver.com), which fetches certificates
automatically:

```caddyfile
portal.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

With nginx, forward the upgrade headers and raise the idle timeout. Senders
may wait up to an hour, and the server pings every 30 s:

```nginx
location /v1 {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 120s;
}
```

## Docker

```console
$ docker build -t portal-rendezvous -f crates/portal-rendezvous/Dockerfile .
$ docker run -d --restart unless-stopped -p 127.0.0.1:8080:8080 portal-rendezvous
```

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--listen` (`PORTAL_RENDEZVOUS_LISTEN`) | `0.0.0.0:8080` | address to listen on |
| `--code-ttl-minutes` | `60` | how long a sender may wait for its receiver |
| `--max-waiting` | `10000` | senders waiting at once; connections are capped at twice this |

`RUST_LOG=debug` shows more detail. Logs never contain codes or payloads.
`GET /` returns `portal-rendezvous` for health checks.

## Limits and abuse

Per connection, the server enforces 8 KiB messages, 8 relayed messages, 10 s
to say hello, and 60 s to finish a pairing. There is no per-IP rate limiting
yet. If you run a public instance, add it at the proxy (for example
`limit_req` in nginx).
