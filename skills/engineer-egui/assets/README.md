# Assets

Font licence texts, a token swatch and a font manifest. No font binaries are
stored here.

To use a font:

1. Read `fonts/manifest.json` for the upstream page and the expected hash.
2. Fetch the pinned font file from that upstream.
3. Verify its SHA-256 against the manifest.
4. Copy the matching `OFL-*.txt` next to the font in the target project.

The hashes were taken from the vendored foilcap fonts on 2026-11-27. The
ArtCraft brand files are not open source and are not included.
