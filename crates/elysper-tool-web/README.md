# elysper-tool-web

The `web_fetch` builtin: the one builtin that talks to the network.

This is the **only** tool crate allowed to pull `reqwest` /
`hyper` / `rustls`. The `make check-mvp-deps` gate asserts that all
seven other tool crates stay offline.

> Plugin crate for `elysper-tool`. See [`SEAMS.md`](../../SEAMS.md) for
> the tool seam.

## CI 契约

- `make check-mvp-deps` 单独允许 `elysper-tool-web` 拉 reqwest；
- `cargo test -p elysper-tool-web --lib` 绿。
