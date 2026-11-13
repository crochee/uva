# uva-attachment

Multimodal attachment store (image / audio / file bytes) with
filesystem + in-memory backends.

> 详细 seam 与公开 API 见 [`uva-attachment/src/lib.rs`](src/lib.rs)
> 与顶层 [`SEAMS.md`](../../SEAMS.md)。本 README 是该 crate 在
> `README` 顶层的索引页。

## 用法

```rust
use uva_attachment::{AttachmentStore, ImageAttachmentRef};

let store = AttachmentStore::in_memory();
let image = ImageAttachmentRef::from_bytes_at(&store, b"...", clock)?;
```

## 依赖方向

- `uva-attachment` → `uva-core`（仅 trait：`Clock` / `Error`）
- 不依赖 `uva-provider` / `uva-tool` / 任何运行时。

## CI 契约

- `cargo test -p uva-attachment --lib` 绿；
- 不引入 `reqwest|hyper|rustls|h2|tower`（`make check-mvp-deps`）。
