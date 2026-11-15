# Source provenance

This project builds on **rsautogui**, Copyright (c) 2026 rsautogui contributors,
MIT licensed. The native Rust library, lazy DLL launcher, original MCP tools,
installation utilities and fixture tests originate from
https://github.com/urtiger101-tw/rsautogui at commit
`1f7b58e4ffdd61e0e3788c7980a142c9b485897a`. The MIT permission and warranty
notice is reproduced in LICENSE and distributed with the release.

The Rust decision contract is adapted from **gemini-decision-1**, Copyright (c)
2026 stevenke1981, MIT licensed, at
https://github.com/stevenke1981/gemini-decision-1 commit
`e724a3bee6dec9d91635c3798400e96b4f0a5e85`. That project credits
luna-decision-core commit `df2a1f187606f4238893dd2059ac7bc49b629e8c` under the same
MIT license. The permission and warranty notice is reproduced in LICENSE.

This is an independent integration, not a Google product. Gemini Decision 1 is
the remote optional provider; rsautogui remains the local Rust input backend.
Transitive Rust dependencies are recorded in Cargo.lock and keep their own
licenses. Release executables are unsigned unless a specific release says otherwise.
