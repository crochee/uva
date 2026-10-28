# elysper-tool-scheduler

The `schedule` tool plugin: let the model create and manage scheduled
jobs on top of `elysper-scheduler::ScheduleStore`.

> Plugin crate for `elysper-tool`. See [`SEAMS.md`](../../SEAMS.md) for
> the tool seam.

The `cron` parser support is opt-in: facade `cron` feature flips both
`elysper-scheduler` and `elysper-tool-scheduler` together so that the
model can actually create cron jobs. Without `cron`, `kind: "cron"` is
refused outright (the parser simply doesn't exist, so partial-feature
configurations can't drift into a half-working state).

## CI 契约

- `make check-mvp-deps` 同时断言 default + `--features cron` 两种配置；
- `cargo test -p elysper-tool-scheduler --lib` 绿；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p elysper-scheduler
  --features cron` 绿（门禁见 `AGENTS.md` §3.6）。
