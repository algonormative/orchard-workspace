# Orchard workspace contribution notes

Keep the server narrow: it owns loopback startup, embedded browser assets, and
bounded shutdown. The host owns records, validation, MCP, credentials, and
subprocess lifetimes. Do not add code that launches, wakes, configures, or
authenticates an agent or provider.

The browser UI must build DOM with text nodes and vetted links. Do not interpolate
workspace records as HTML. A background refresh must patch data in place; it
must not discard a focused composer, reply context, or a reader's scroll position.
Every block-code surface uses the shared copyable-code renderer: it writes source
with `textContent`, copies that exact source, and contains horizontal scrolling
inside the block so it can never widen the page. Do not add one-off copy buttons.
Every secondary UI must offer a visible close, cancel, or back action and Escape;
browser Back restores in-app context without serializing drafts or credentials.
Resource UI uses canonical workspace-relative resource references and treats
incoming hrefs as untrusted: parse only Orchard routes for the active workspace.
The main surface is one resource tree plus one tabbed viewer, never a permanent
metadata or details column.
Artifact content is a read-only Markdown, code, or safe-raster viewer with a
download fallback. Do not add an embedded editor.
Use `npm --prefix ui run test:browser` for the local Playwright fixture. The
backend-owned live-server smoke exercises the embedded Rust bundle separately.

The Beads source is an immutable, reviewed vendor snapshot. Do not replace it
with a source path outside this repository or an upstream HEAD. Run the small
UI smoke and the relevant Cargo checks before changing packaging behavior. A
release package contains one `orchard` executable, `resources/bin/br`, and the
checked-in notices; it must not depend on Node, Python, Git, or `PATH` at runtime.
