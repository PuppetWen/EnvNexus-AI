use std::process::ExitCode;

fn main() -> ExitCode {
    if let Some(exit_code) = envnexus_ai_lib::application_update::helper_exit_code_from_args() {
        return exit_code;
    }
    if std::env::args_os().len() > 1 {
        return envnexus_ai_lib::cli::main();
    }
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
    envnexus_ai_lib::run();
    ExitCode::SUCCESS
}
