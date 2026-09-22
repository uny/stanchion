//! The prompt helper as a binary of its own: `stanchion-prompt-helper <socket>`. The body
//! is `stanchion_core::prompt_helper`; the application runs the same body as a mode of
//! its own executable, and this binary is what the tests drive.

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(socket), None) = (args.next(), args.next()) else {
        eprintln!("usage: stanchion-prompt-helper <socket>");
        std::process::exit(2);
    };
    stanchion_core::prompt_helper::serve(socket.into());
}
