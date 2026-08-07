use std::process::ExitCode;

use bangbang_cpu_template_helper::cli::run_cli_with_providers;
use bangbang_cpu_template_helper::host::SystemHostFingerprintProvider;
use bangbang_cpu_template_helper::provider::HvfEffectiveCpuTemplateProvider;

fn main() -> ExitCode {
    let mut host_provider = SystemHostFingerprintProvider::new();
    let mut effective_provider = HvfEffectiveCpuTemplateProvider::new();
    let exit = run_cli_with_providers(
        std::env::args_os(),
        &mut host_provider,
        &mut effective_provider,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(exit.code())
}
