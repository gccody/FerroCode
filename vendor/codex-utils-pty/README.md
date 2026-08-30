# Vendored Codex PTY utility

This crate is copied from `openai/codex` revision
`6478a751fde8884b2fdc76486fe23175a8e795d4` and is used through Cargo's
source patch mechanism.

The vendored copy carries a minimal Windows compatibility fix: ConPTY handles
are cast explicitly between the `winapi` and standard-library `c_void` pointer
types. The upstream source otherwise remains unchanged.
