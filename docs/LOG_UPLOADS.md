# Log uploads

Where the launcher's log bundles go. Ember's report intake (`ember-reports`) and its Bugsink belong to the SF4
Ember owner and are not used. The launcher sends to a small service of our own, `ember-logs`, on the Hetzner box
that already runs the EU relay and the frame-data bot.

## What the player sees

On the Logs page: pick a game or all games, optionally type what went wrong, press **Send**. The launcher shows
what goes in (files, sizes) before sending. After a send it shows a report code such as `L-7F3K-Q2MX`, to paste in
Discord `#support`. The page says the logs are kept 90 days and read only by the Ember developers.

## The request

`POST https://logs.fraccie.be:8443/v1/logs`, multipart, two parts:

| Part | Content |
|---|---|
| `meta` | JSON: `schema` 1, `launcher` version, `os` (`windows`, `linux`, `steamdeck`), `games` (id, channel, mod tag per game), `note` (at most 1,000 characters), `bundle_sha256` |
| `bundle` | the zip the launcher already makes (`manifest.json` plus one folder per game) |

Answer: `201 {"code":"L-7F3K-Q2MX"}`. Refusals: `413` too large, `422` not a valid bundle, `429` too many, `507`
storage full. The launcher keeps a refused bundle in `logs/outbox`; it moves to `logs/sent` only on a 201, as it
does now.

## Limits

- Bundle at most 25 MB (logs compress about 10 to 1; the launcher already caps each game at 30 files and 32 MB
  before compression). Request at most 26 MB, cut off while reading.
- Per IP: 5 uploads an hour, 20 a day. No account or sign-in, so a player can send before Ember ID works.
- The zip is checked, never unpacked on the server: at most 200 entries, names only `manifest.json` and
  `<game>/<file>` with `.log`, `.txt`, `.json` or `.dmp`, no `..` or absolute paths, at most 128 MB unpacked
  in total (no zip bombs), sha256 equal to `bundle_sha256`.
- Disk: a fixed budget (15 GB; the box has 38 GB). When it is used up, new uploads get `507` and the Discord channel is told once.

## Storage and reading

- Saved as `/var/lib/ember-logs/<code>.zip` with `<code>.json` (meta, time, size; never the IP address).
  Removed after 90 days by the service's own hourly sweep.
- Each upload posts one message to a private Discord channel through a webhook: code, games and versions, OS,
  size, the note, and a download link `https://logs.fraccie.be:8443/v1/logs/<code>?k=<hmac>`. The link is signed with a key
  only the server holds, so a code alone downloads nothing.
- No other reading route. No listing.

## Privacy

- The launcher already takes the Windows or Linux account name out of paths before zipping.
- The server stores no IP address; the rate limit keeps hashed IPs in memory only.
- Nothing is sent without pressing Send. There is no automatic upload.

## Build and run

- One small Rust binary (`server/ember-logs` in this repo, axum, no database) on 127.0.0.1:8790, a systemd
  unit, nginx in front on port 8443 with a Let's Encrypt certificate.
- Ports 80 and 443 belong to the EU relay (`iroh-relay`, which does its own TLS on 443). nginx answers port 80:
  the certificate's web check for `logs.fraccie.be`, everything else goes on to the relay's plain-HTTP port,
  moved to 127.0.0.1:3340. The relay's 443 and QUIC are unchanged.
- Config: listen address, storage folder, disk budget, webhook URL, signing key (environment file, `0600`, not
  in git).
- Tests: the zip checks (bad names, too many entries, a bomb, wrong sha256), the size cut-off, the rate limit,
  the sweep, the signed link.
- The catalog's `reports.url` points at it once it runs; until then the launcher keeps saving bundles locally.

## Decided

- Host `logs.fraccie.be` (A and AAAA to the Hetzner box), port 8443.
- One message per bundle in a new private `#launcher-logs` channel in the Ember Discord.
- Crash dumps (`.dmp`) are allowed in the zip; the 25 MB cap still holds.
