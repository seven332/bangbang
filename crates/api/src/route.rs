#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    DescribeInstance,
    Version,
    VmState,
    VmConfig,
    Actions,
    BootSource,
    CpuConfig,
    Logger,
    MachineConfig,
    Metrics,
    Mmds,
    Drive,
    NetworkInterface,
    Vsock,
}
