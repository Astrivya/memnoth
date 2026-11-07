//! `memnoth` — see `lib.rs` for the whole product. This file is the door.

fn main() -> std::process::ExitCode {
    // clap builds the whole verb tree recursively, and a debug build's frames are
    // large enough that the command tree alone overflows Windows' 1 MB main
    // stack before the first verb runs (`memnoth --version` died with
    // STATUS_STACK_OVERFLOW). Give the one worker that runs the product room;
    // nothing about the CLI changes, and the main thread only waits.
    let worker = std::thread::Builder::new()
        .name("memnoth".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(memnoth::run);
    match worker {
        Ok(worker) => worker.join().unwrap_or(std::process::ExitCode::FAILURE),
        // Spawning a thread can only fail when the process cannot allocate one;
        // running inline would then hit the same stack limit, so report it.
        Err(err) => {
            eprintln!("memnoth: cannot start: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
