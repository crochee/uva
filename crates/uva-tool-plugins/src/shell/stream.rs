//! The `shell` tool's streaming entry point.
//!
//! `Tool::stream` is the dispatcher's primary path: every `Progress`
//! item is forwarded to the agent loop (and from there to the client
//! and the durable log), and the single `Result` item is the tool's
//! output. The command itself runs unchanged ([`super::ShellTool`]'s
//! `run`); this module only decides *when* its pipe chunks are yielded.
//!
//! The order is the contract: any number of `Progress` items, then
//! exactly one `Result`. A chunk the run already produced is never
//! dropped because the run resolved first — the pipe readers finish
//! before `run` returns, so whatever they left buffered is drained
//! before the result settles.

use std::pin::Pin;

use futures::{
    StreamExt,
    channel::mpsc::TryRecvError,
    future::Either,
    pin_mut,
    stream,
};
use uva_tool::{Context, StreamOutput, Tool, ToolOutput};

use super::ShellTool;

/// Drive one command as a tool stream: each pipe chunk is a
/// `Progress` item, the rendered run is the single `Result`.
///
/// `Tool::call` runs the same command without a progress sink; a
/// caller that only needs the output should use it. Both bound their
/// output the way the trait requires of a `stream` override — the
/// dispatcher never truncates what a tool yields.
pub(super) fn run<'a>(
    tool: &'a ShellTool,
    input: serde_json::Value,
    context: &'a Context,
) -> Pin<Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>> {
    let (progress, chunks) = futures::channel::mpsc::unbounded();
    let run: Pin<Box<dyn Future<Output = ToolOutput> + Send + 'a>> =
        Box::pin(tool.run(input, context, Some(progress)));

    Box::pin(stream::unfold(
        Some(Phase::Running { run, chunks }),
        move |state| async move {
            let mut phase = state?;
            loop {
                match phase {
                    Phase::Running {
                        mut run,
                        mut chunks,
                    } => {
                        match chunks.try_recv() {
                            Ok(chunk) => {
                                return Some((
                                    StreamOutput::Progress(
                                        bounded(tool, chunk, context).await,
                                    ),
                                    Some(Phase::Running { run, chunks }),
                                ));
                            }
                            Err(TryRecvError::Closed) => {
                                let output = run.await;
                                return Some((
                                    StreamOutput::Result(
                                        bounded(tool, output, context).await,
                                    ),
                                    None,
                                ));
                            }
                            // Open and empty: wait for either side.
                            Err(TryRecvError::Empty) => {}
                        }
                        let next = chunks.next();
                        pin_mut!(next);
                        match futures::future::select(run.as_mut(), next).await
                        {
                            Either::Left((output, _)) => {
                                phase = Phase::Draining { output, chunks };
                            }
                            Either::Right((Some(chunk), _)) => {
                                return Some((
                                    StreamOutput::Progress(
                                        bounded(tool, chunk, context).await,
                                    ),
                                    Some(Phase::Running { run, chunks }),
                                ));
                            }
                            Either::Right((None, _)) => {
                                let output = run.await;
                                return Some((
                                    StreamOutput::Result(
                                        bounded(tool, output, context).await,
                                    ),
                                    None,
                                ));
                            }
                        }
                    }
                    Phase::Draining { output, mut chunks } => {
                        match chunks.try_recv() {
                            Ok(chunk) => {
                                return Some((
                                    StreamOutput::Progress(
                                        bounded(tool, chunk, context).await,
                                    ),
                                    Some(Phase::Draining { output, chunks }),
                                ));
                            }
                            // The run returned, so its readers are
                            // done: the buffer is all that is left.
                            Err(TryRecvError::Empty | TryRecvError::Closed) => {
                                return Some((
                                    StreamOutput::Result(
                                        bounded(tool, output, context).await,
                                    ),
                                    None,
                                ));
                            }
                        }
                    }
                }
            }
        },
    ))
}

/// One output bounded the way the trait requires of a stream
/// override.
async fn bounded(
    tool: &ShellTool,
    mut output: ToolOutput,
    context: &Context,
) -> ToolOutput {
    tool.truncate(&mut output, context).await;
    output
}

/// One step of the shell stream's state machine.
///
/// `Running` races the command against its chunks; once the command
/// has returned, `Draining` emits the chunks it left buffered before
/// the single terminal result.
enum Phase<'a> {
    Running {
        run: Pin<Box<dyn Future<Output = ToolOutput> + Send + 'a>>,
        chunks: futures::channel::mpsc::UnboundedReceiver<ToolOutput>,
    },
    Draining {
        output: ToolOutput,
        chunks: futures::channel::mpsc::UnboundedReceiver<ToolOutput>,
    },
}
