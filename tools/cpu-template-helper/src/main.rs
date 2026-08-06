use std::process::ExitCode;

use bangbang_cpu_template_helper::cli::run_cli_with_provider;
use bangbang_cpu_template_helper::provider::HvfEffectiveCpuTemplateProvider;

fn main() -> ExitCode {
    let mut provider = HvfEffectiveCpuTemplateProvider::new();
    let exit = run_cli_with_provider(
        std::env::args_os(),
        &mut provider,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(exit.code())
}
