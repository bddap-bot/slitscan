use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("slitscan=info,wgpu=warn"),
    )
    .init();
    let args = match slitscan::args::parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            print!("{}", slitscan::args::usage());
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprintln!("slitscan: {why}\n\n{}", slitscan::args::usage());
            return ExitCode::FAILURE;
        }
    };
    if let Err(why) = slitscan::app::run(args) {
        eprintln!("slitscan: {why}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
