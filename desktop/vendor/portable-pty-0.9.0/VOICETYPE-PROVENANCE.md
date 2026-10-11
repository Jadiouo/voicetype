# portable-pty 0.9.0 source

This directory contains the crates.io `portable-pty` 0.9.0 source archive,
SHA-256 `b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e`.
Its upstream license is MIT (`LICENSE.md`). Cargo registry extraction metadata and
the crate's standalone lockfile were removed; all distributed Rust source and
the package manifest remain.

The only source change is in `src/win/psuedocon.rs`: remove the
`PSUEDOCONSOLE_INHERIT_CURSOR` flag from `CreatePseudoConsole`. VoiceType does
not render a parent console or implement the cursor-position query/reply
protocol. Its output thread drains the ConPTY output. Microsoft documents that
closing a pseudoconsole with this flag while not answering the query can
deadlock: https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session

The other two flags, `PSEUDOCONSOLE_RESIZE_QUIRK` and
`PSEUDOCONSOLE_WIN32_INPUT_MODE`, remain unchanged from upstream.
