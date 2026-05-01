use std::env;
use std::io;
use std::process::{Command, ExitStatus, Output};
use std::sync::LazyLock;
use std::time::Instant;

static MEASURE_COMMAND_PERF: LazyLock<bool> = LazyLock::new(||
    env::var("GITAI_MEASURE_COMMAND_PERF").is_ok()
);

pub trait MeasuredCommand {
    fn measured_output(&mut self) -> io::Result<Output>;
    fn measured_status(&mut self) -> io::Result<ExitStatus>;
}

impl MeasuredCommand for Command {
    fn measured_output(&mut self) -> io::Result<Output> {
        if *MEASURE_COMMAND_PERF {
            let cmd_program = format!("{:?}", self.get_program());
            let cmd_args = format!("{:?}", self.get_args().collect::<Vec<_>>());
            let start = Instant::now();
            let output = self.output();
            let elapsed_ms = start.elapsed().as_millis();
            eprintln!("[perf] {{\"elapsed_ms\": {elapsed_ms}, \"cmd_program\": {cmd_program}, \"cmd_args\": {cmd_args}}}");
            output
        } else {
            self.output()
        }
    }

    fn measured_status(&mut self) -> io::Result<ExitStatus> {
        if *MEASURE_COMMAND_PERF {
            let cmd_program = format!("{:?}", self.get_program());
            let cmd_args = format!("{:?}", self.get_args().collect::<Vec<_>>());
            let start = Instant::now();
            let status = self.status();
            let elapsed_ms = start.elapsed().as_millis();
            eprintln!("[perf] {{\"elapsed_ms\": {elapsed_ms}, \"cmd_program\": {cmd_program}, \"cmd_args\": {cmd_args}}}");
            status
        } else {
            self.status()
        }
    }
}
