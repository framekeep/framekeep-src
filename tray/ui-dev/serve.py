"""Static server for the dev harness that never serves a stale file.

`python -m http.server` sends no `Cache-Control`, so the browser applies its own
freshness heuristic and happily reuses a module or a stylesheet it fetched a
minute ago. That cost three false readings in one session on 18/08 -- twice a
CSS fix and once a JS fix were measured as "no change" when what was running was
the file from before the edit.

The first attempt at a cure was worse than the disease: stamping `?t=<now>` onto
the URLs the harness imports. It works for those, but a module's own relative
imports do not inherit the query, so `review.js` importing `./queue.js` loaded a
SECOND copy of the module the harness had already loaded as `queue.js?t=...`.
Two instances, two sets of listeners, every click handled twice -- caught when a
menu item fired its command twice in a row.

Caching is an HTTP problem, so it is fixed with an HTTP header, where module
identity is not involved at all.

    python tray/ui-dev/serve.py [port]
"""

import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class NoCacheHandler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store, must-revalidate")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        super().end_headers()

    def log_message(self, fmt, *args):
        # The default logs every asset; the harness pulls dozens per load and
        # the noise buries the one line that matters when something 404s.
        if "404" in (fmt % args):
            super().log_message(fmt, *args)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    print(f"harness on http://localhost:{port}/tray/ui-dev/ (no-store)")
    ThreadingHTTPServer(("127.0.0.1", port), NoCacheHandler).serve_forever()
